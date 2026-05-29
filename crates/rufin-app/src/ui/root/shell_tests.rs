use super::right_panel::{
    clamp_queue_lyrics_position, queue_lyrics_default_position, queue_lyrics_initial_position,
    queue_lyrics_position_from_ratio, queue_lyrics_position_ratio,
};
use super::{
    AutoLyricsRequest, PlaylistEntryListState, PlaylistEntrySort, SnapshotRenderDecision,
    auto_lyrics_request_for_settings, auto_lyrics_skip_action_enabled,
    cover::record_cover_path_lookup_request, current_playback_track_id,
    home_visible_sections::changed_visible_home_section_kinds, playlist_drop_index,
    playlist_entries_for_state, seekbar_target_seconds, snapshot_event_outcome,
};
use rufin_core::{
    Album, AlbumId, AppSettings, ArtistId, HomeSection, HomeSectionKind, LibrarySourceSelection,
    QueueEntry, QueueEntryId, Route, SearchKind, Track, TrackId, TrackSortKey, TrackTableSettings,
};
use rufin_provider::{LyricLine, Lyrics, LyricsSource, PlaylistEntry};
use std::collections::HashMap;
#[test]
pub(in crate::ui) fn detail_cover_lookup_can_reuse_prefetched_grid_cover() {
    let candidates = super::decoded_cover_candidate_sizes(super::DETAIL_COVER_SIZE);

    assert!(candidates.contains(&super::DETAIL_COVER_SIZE));
    assert!(candidates.contains(&super::GRID_COVER_SIZE));
    assert!(
        candidates
            .iter()
            .position(|size| *size == super::DETAIL_COVER_SIZE)
            < candidates
                .iter()
                .position(|size| *size == super::GRID_COVER_SIZE)
    );
}
#[test]
pub(in crate::ui) fn visible_cover_lookup_reuses_and_upgrades_warm_lookup() {
    let mut lookups = HashMap::new();

    assert!(record_cover_path_lookup_request(
        &mut lookups,
        "album-art".to_string(),
        super::CoverPathLookupIntent::Warm,
    ));
    assert!(!record_cover_path_lookup_request(
        &mut lookups,
        "album-art".to_string(),
        super::CoverPathLookupIntent::Visible,
    ));
    assert_eq!(
        lookups.get("album-art"),
        Some(&super::CoverPathLookupIntent::Visible)
    );

    assert!(record_cover_path_lookup_request(
        &mut lookups,
        "now-playing".to_string(),
        super::CoverPathLookupIntent::Visible,
    ));
    assert!(!record_cover_path_lookup_request(
        &mut lookups,
        "now-playing".to_string(),
        super::CoverPathLookupIntent::Warm,
    ));
    assert_eq!(
        lookups.get("now-playing"),
        Some(&super::CoverPathLookupIntent::Visible)
    );
}
#[test]
pub(in crate::ui) fn home_section_pages_reset_for_new_home_data() {
    let mut states = HashMap::from([(
        HomeSectionKind::Explore,
        super::HomeSectionState {
            page_start: 6,
            page_size: 3,
        },
    )]);

    super::reset_home_section_pages(&mut states);

    assert!(states.is_empty());
}
#[test]
pub(in crate::ui) fn home_refresh_targets_only_changed_visible_sections() {
    let explore = test_home_album_section(HomeSectionKind::Explore, 1);
    let most_played = test_home_album_section(HomeSectionKind::MostPlayed, 2);
    let previous = vec![explore.clone(), most_played.clone()];
    let mut changed_explore = explore.clone();
    changed_explore.albums[0].title = "Different explore album".to_string();
    let mut changed_most_played = most_played.clone();
    changed_most_played.albums[0].title = "Different most played album".to_string();
    let sections = vec![changed_explore, changed_most_played];
    let visible = vec![
        HomeSectionKind::Explore,
        HomeSectionKind::MostPlayed,
        HomeSectionKind::RecentlyPlayed,
    ];

    assert_eq!(
        changed_visible_home_section_kinds(visible.clone(), &previous, &sections, false),
        vec![HomeSectionKind::MostPlayed]
    );
    assert_eq!(
        changed_visible_home_section_kinds(visible, &previous, &sections, true),
        vec![HomeSectionKind::Explore, HomeSectionKind::MostPlayed]
    );
}
#[test]
pub(in crate::ui) fn snapshot_event_outcome_prioritizes_first_run_completion() {
    let previous_source = None;
    let next_source = Some(LibrarySourceSelection::Local);

    let outcome = snapshot_event_outcome(true, false, &previous_source, &next_source, true, true);

    assert_eq!(outcome.render, SnapshotRenderDecision::FirstRunFinished);
    assert!(!outcome.entered_first_run);
}
#[test]
pub(in crate::ui) fn snapshot_event_outcome_navigates_when_source_changes() {
    let previous_source = None;
    let next_source = Some(LibrarySourceSelection::Local);

    let outcome =
        snapshot_event_outcome(false, false, &previous_source, &next_source, false, false);

    assert_eq!(outcome.render, SnapshotRenderDecision::SourceChanged);
    assert!(!outcome.entered_first_run);
}
#[test]
pub(in crate::ui) fn snapshot_event_outcome_preserves_scroll_for_stable_source() {
    let source = Some(LibrarySourceSelection::Local);

    let outcome = snapshot_event_outcome(false, false, &source, &source, false, false);

    assert_eq!(outcome.render, SnapshotRenderDecision::PreserveScroll);
    assert!(!outcome.entered_first_run);
}
#[test]
pub(in crate::ui) fn snapshot_event_outcome_marks_first_run_entry() {
    let source = None::<LibrarySourceSelection>;

    let outcome = snapshot_event_outcome(false, true, &source, &source, false, false);

    assert_eq!(outcome.render, SnapshotRenderDecision::PreserveScroll);
    assert!(outcome.entered_first_run);
}
#[test]
pub(in crate::ui) fn manual_ui_perf_observer_records_scrolls_by_route() {
    let monitor = super::UiPerfMonitor::new(super::UiPerfOptions {
        max_gap_ms: 120,
        route_ms: 650,
        duration_ms: 15_000,
        asset_ms: 300,
        require_assets: false,
        terminal_events: false,
        observe_scroll: true,
        output: None,
    });

    monitor.record_manual_scroll_step("Tracks", 10.0, 100.0);
    monitor.record_manual_scroll_step("Tracks", 40.0, 100.0);
    monitor.record_manual_scroll_step("Albums", 5.0, 50.0);
    monitor.finish_scroll();

    let report = monitor.report();
    assert!(report.contains("RUFIN_PERF_SCROLL route=Tracks scenario=manual"));
    assert!(report.contains("steps=2"));
    assert!(report.contains("max_adjustment=100"));
    assert!(report.contains("RUFIN_PERF_SCROLL route=Albums scenario=manual"));
}
#[test]
pub(in crate::ui) fn ui_perf_plan_keeps_home_out_of_the_critical_window() {
    let plan = super::ui_perf_take_plan(
        vec![
            (Route::Tracks, super::UiPerfScenario::HumanScroll),
            (Route::Tracks, super::UiPerfScenario::FastScroll),
            (Route::Albums, super::UiPerfScenario::HumanScroll),
        ],
        vec![Route::Artists, Route::Home],
        2_000,
        500,
    );

    let routes = plan
        .iter()
        .map(|(route, _)| route.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        routes,
        vec![Route::Tracks, Route::Tracks, Route::Albums, Route::Artists]
    );
}
#[test]
pub(in crate::ui) fn ui_perf_route_render_budget_is_not_the_scroll_gap_budget() {
    let monitor = super::UiPerfMonitor::new(super::UiPerfOptions {
        max_gap_ms: 120,
        route_ms: 650,
        duration_ms: 15_000,
        asset_ms: 300,
        require_assets: true,
        terminal_events: false,
        observe_scroll: false,
        output: None,
    });

    monitor.record_route_render("Albums".to_string(), std::time::Duration::from_millis(300));
    monitor.record_cover_cache_hit("cached-cover");

    assert!(!monitor.failed());
}
#[test]
pub(in crate::ui) fn ui_perf_scroll_failure_allows_one_borderline_tick() {
    let monitor = super::UiPerfMonitor::new(super::UiPerfOptions {
        max_gap_ms: 120,
        route_ms: 650,
        duration_ms: 15_000,
        asset_ms: 300,
        require_assets: true,
        terminal_events: false,
        observe_scroll: false,
        output: None,
    });

    monitor.record_cover_cache_hit("cached-cover");
    monitor
        .inner
        .borrow_mut()
        .route_scrolls
        .push(super::UiPerfRouteScroll {
            route: "Tracks".to_string(),
            scenario: "drag_sweep",
            elapsed_ms: 500,
            steps: 12,
            max_gap_ms: 126,
            over_budget_ticks: 1,
            max_adjustment: 1_000.0,
            min_value: 0.0,
            max_value: 50.0,
            covers_ready: 0,
            decoded_covers: 0,
        });
    assert!(!monitor.failed());

    monitor
        .inner
        .borrow_mut()
        .route_scrolls
        .push(super::UiPerfRouteScroll {
            route: "Tracks".to_string(),
            scenario: "drag_sweep",
            elapsed_ms: 500,
            steps: 12,
            max_gap_ms: 300,
            over_budget_ticks: 1,
            max_adjustment: 1_000.0,
            min_value: 0.0,
            max_value: 50.0,
            covers_ready: 0,
            decoded_covers: 0,
        });
    assert!(monitor.failed());
}
#[test]
pub(in crate::ui) fn ui_perf_scroll_failure_ignores_nearly_static_routes() {
    let monitor = super::UiPerfMonitor::new(super::UiPerfOptions {
        max_gap_ms: 120,
        route_ms: 650,
        duration_ms: 15_000,
        asset_ms: 300,
        require_assets: true,
        terminal_events: false,
        observe_scroll: false,
        output: None,
    });
    let tiny_scroll = super::UiPerfRouteScroll {
        route: "Playlists".to_string(),
        scenario: "human_scroll",
        elapsed_ms: 800,
        steps: 0,
        max_gap_ms: 650,
        over_budget_ticks: 3,
        max_adjustment: 97.0,
        min_value: 0.0,
        max_value: 97.0,
        covers_ready: 0,
        decoded_covers: 0,
    };
    assert!(!monitor.scroll_sample_failed(&tiny_scroll));
    let meaningful_scroll = super::UiPerfRouteScroll {
        max_adjustment: 1_000.0,
        ..tiny_scroll
    };
    assert!(monitor.scroll_sample_failed(&meaningful_scroll));
}
#[test]
pub(in crate::ui) fn queue_lyrics_position_clamps_to_available_height() {
    assert_eq!(clamp_queue_lyrics_position(800, 1701), 500);
    assert_eq!(clamp_queue_lyrics_position(800, 10), 120);
    assert_eq!(clamp_queue_lyrics_position(200, 1701), 120);
    assert_eq!(queue_lyrics_default_position(700), 400);
    assert_eq!(queue_lyrics_default_position(1400), 1000);
    assert_eq!(queue_lyrics_initial_position(700, None), 400);
    assert_eq!(queue_lyrics_initial_position(700, Some(0.5)), 350);
    assert_eq!(queue_lyrics_initial_position(700, Some(2.0)), 400);
    assert_eq!(queue_lyrics_initial_position(700, Some(f64::NAN)), 400);
    assert_eq!(queue_lyrics_position_from_ratio(700, 0.5), 350);
    assert_eq!(queue_lyrics_position_ratio(700, 350), 0.5);
    let saved_default_ratio = queue_lyrics_position_ratio(700, 400);
    assert_eq!(
        queue_lyrics_initial_position(1400, Some(saved_default_ratio)),
        800
    );
}
#[test]
pub(in crate::ui) fn current_playback_track_id_uses_restored_current_entry() {
    let track_id = TrackId::fake(7);
    let snapshot = super::PlaybackSnapshot {
        current: Some(QueueEntry {
            id: QueueEntryId::new("queue-7"),
            track_id: track_id.clone(),
            album_id: None,
            title: "Restored".to_string(),
            artist: "Artist".to_string(),
            artist_id: None,
            album: "Album".to_string(),
            year: 2026,
            duration_seconds: 180,
            favorite: false,
            image_ref: None,
            local_path: None,
            source_format: None,
        }),
        ..super::PlaybackSnapshot::default()
    };

    assert_eq!(current_playback_track_id(&snapshot), Some(track_id));
    assert_eq!(
        current_playback_track_id(&super::PlaybackSnapshot::default()),
        None
    );
}
#[test]
pub(in crate::ui) fn playlist_entry_search_and_sort_use_track_fields() {
    let mut first = test_track("Artist B", None);
    first.title = "Alpha".to_string();
    first.album = "Plain Album".to_string();
    first.duration_seconds = 240;
    let mut second = test_track("Artist A", None);
    second.id = TrackId::fake(2);
    second.title = "Beta".to_string();
    second.album = "Needle Album".to_string();
    second.duration_seconds = 120;
    let entries = vec![
        PlaylistEntry {
            entry_id: "entry-alpha".to_string(),
            track: first,
        },
        PlaylistEntry {
            entry_id: "entry-beta".to_string(),
            track: second,
        },
    ];

    let filtered = playlist_entries_for_state(
        &entries,
        &PlaylistEntryListState {
            query: "needle".to_string(),
            sort: PlaylistEntrySort::Order,
            descending: false,
        },
    );
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].1.entry_id, "entry-beta");

    let sorted = playlist_entries_for_state(
        &entries,
        &PlaylistEntryListState {
            query: String::new(),
            sort: PlaylistEntrySort::Duration,
            descending: true,
        },
    );
    assert_eq!(sorted[0].1.entry_id, "entry-alpha");
    assert_eq!(sorted[1].1.entry_id, "entry-beta");
}
#[test]
pub(in crate::ui) fn playlist_drop_index_accounts_for_removed_source_row() {
    let entries = ["a", "b", "c"]
        .into_iter()
        .enumerate()
        .map(|(index, entry_id)| {
            let mut track = test_track("Artist", None);
            track.id = TrackId::fake(index + 1);
            PlaylistEntry {
                entry_id: entry_id.to_string(),
                track,
            }
        })
        .collect::<Vec<_>>();

    assert_eq!(playlist_drop_index(&entries, "a", 2, false), Some(1));
    assert_eq!(playlist_drop_index(&entries, "a", 2, true), Some(2));
    assert_eq!(playlist_drop_index(&entries, "c", 0, false), Some(0));
    assert_eq!(playlist_drop_index(&entries, "b", 1, false), None);
}
#[test]
pub(in crate::ui) fn track_artist_route_prefers_detail_and_falls_back_to_artist_search() {
    let track = test_track("Track Artist", Some(ArtistId::fake(3)));
    assert_eq!(
        super::track_artist_route(&track),
        Some(Route::ArtistDetail(ArtistId::fake(3)))
    );

    let track = test_track("Loose Artist", None);
    assert_eq!(
        super::track_artist_route(&track),
        Some(Route::Search {
            query: "Loose Artist".to_string(),
            kind: SearchKind::Artists,
        })
    );

    assert_eq!(super::track_artist_route(&test_track("   ", None)), None);
}
#[test]
pub(in crate::ui) fn album_artist_route_prefers_detail_and_falls_back_to_artist_search() {
    let album = test_album("Album Artist", Some(ArtistId::fake(5)));
    assert_eq!(
        super::album_artist_route(&album),
        Some(Route::ArtistDetail(ArtistId::fake(5)))
    );

    let album = test_album("Compilation Artist", None);
    assert_eq!(
        super::album_artist_route(&album),
        Some(Route::Search {
            query: "Compilation Artist".to_string(),
            kind: SearchKind::Artists,
        })
    );

    assert_eq!(super::album_artist_route(&test_album("", None)), None);
}
#[test]
pub(in crate::ui) fn compact_artist_track_sort_keeps_favorites_first() {
    let mut favorite_late = test_track("Artist", Some(ArtistId::fake(1)));
    favorite_late.id = TrackId::fake(1);
    favorite_late.title = "Zulu".to_string();
    favorite_late.favorite = true;
    let mut ordinary_first = test_track("Artist", Some(ArtistId::fake(1)));
    ordinary_first.id = TrackId::fake(2);
    ordinary_first.title = "Alpha".to_string();
    let mut favorite_first = test_track("Artist", Some(ArtistId::fake(1)));
    favorite_first.id = TrackId::fake(3);
    favorite_first.title = "Bravo".to_string();
    favorite_first.favorite = true;

    let mut tracks = vec![
        ordinary_first.clone(),
        favorite_late.clone(),
        favorite_first.clone(),
    ];
    let settings = TrackTableSettings {
        sort_key: TrackSortKey::Title,
        ..TrackTableSettings::default()
    };

    super::sort_tracks_with_options(&mut tracks, &settings, true);

    assert_eq!(
        tracks
            .iter()
            .map(|track| track.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Bravo", "Zulu", "Alpha"]
    );
}
#[test]
pub(in crate::ui) fn full_artist_track_sort_uses_selected_ranking() {
    let mut favorite_late = test_track("Artist", Some(ArtistId::fake(1)));
    favorite_late.id = TrackId::fake(1);
    favorite_late.title = "Zulu".to_string();
    favorite_late.favorite = true;
    let mut ordinary_first = test_track("Artist", Some(ArtistId::fake(1)));
    ordinary_first.id = TrackId::fake(2);
    ordinary_first.title = "Alpha".to_string();
    let mut favorite_first = test_track("Artist", Some(ArtistId::fake(1)));
    favorite_first.id = TrackId::fake(3);
    favorite_first.title = "Bravo".to_string();
    favorite_first.favorite = true;

    let mut tracks = vec![favorite_late, ordinary_first, favorite_first];
    let settings = TrackTableSettings {
        sort_key: TrackSortKey::Title,
        ..TrackTableSettings::default()
    };

    super::sort_tracks_with_options(&mut tracks, &settings, false);

    assert_eq!(
        tracks
            .iter()
            .map(|track| track.title.as_str())
            .collect::<Vec<_>>(),
        vec!["Alpha", "Bravo", "Zulu"]
    );
}
#[test]
pub(in crate::ui) fn artist_discography_uses_responsive_cards() {
    assert!(super::route_uses_responsive_cards(
        &Route::ArtistDiscography(ArtistId::fake(1))
    ));
}
#[test]
pub(in crate::ui) fn route_boundary_keeps_route_items_inside_main_pane() {
    let spec = super::route_boundary_spec();

    assert_eq!(spec.horizontal_policy, gtk::PolicyType::Automatic);
    assert_eq!(spec.vertical_policy, gtk::PolicyType::Never);
    assert_eq!(spec.overflow, gtk::Overflow::Hidden);
    assert_eq!(spec.min_content_width, 0);
    assert!(!spec.propagate_natural_width);
    assert!(spec.hexpand);
    assert!(spec.vexpand);
}
#[test]
pub(in crate::ui) fn seekbar_target_seconds_uses_committed_clamped_value() {
    assert_eq!(seekbar_target_seconds(42.4, 180), 42);
    assert_eq!(seekbar_target_seconds(42.5, 180), 43);
    assert_eq!(seekbar_target_seconds(-10.0, 180), 0);
    assert_eq!(seekbar_target_seconds(220.0, 180), 180);
    assert_eq!(seekbar_target_seconds(f64::NAN, 180), 0);
}
#[test]
pub(in crate::ui) fn auto_lyrics_skip_action_only_enabled_for_unsuppressed_external_tracks() {
    let track_id = TrackId::fake(11);
    let mut settings = AppSettings {
        external_lyrics_enabled: true,
        ..AppSettings::default()
    };

    assert!(auto_lyrics_skip_action_enabled(
        &settings,
        Some(&track_id),
        None
    ));

    settings
        .suppressed_auto_lyrics_track_ids
        .push(track_id.as_str().to_string());
    assert!(!auto_lyrics_skip_action_enabled(
        &settings,
        Some(&track_id),
        None
    ));

    settings.suppressed_auto_lyrics_track_ids.clear();
    settings.external_lyrics_enabled = false;
    assert!(!auto_lyrics_skip_action_enabled(
        &settings,
        Some(&track_id),
        None
    ));

    settings.external_lyrics_enabled = true;
    settings.private_mode = true;
    assert!(!auto_lyrics_skip_action_enabled(
        &settings,
        Some(&track_id),
        None
    ));
    assert!(!auto_lyrics_skip_action_enabled(&settings, None, None));
}
#[test]
pub(in crate::ui) fn auto_lyrics_skip_action_is_hidden_for_server_lyrics() {
    let track_id = TrackId::fake(13);
    let settings = AppSettings {
        external_lyrics_enabled: true,
        ..AppSettings::default()
    };
    let server_lyrics = Lyrics {
        track_id: track_id.clone(),
        source: LyricsSource::Server,
        lines: vec![LyricLine {
            text: "server line".to_string(),
            start_millis: None,
        }],
    };
    let remote_lyrics = Lyrics {
        track_id: track_id.clone(),
        source: LyricsSource::Remote,
        lines: vec![LyricLine {
            text: "remote line".to_string(),
            start_millis: None,
        }],
    };

    assert!(!auto_lyrics_skip_action_enabled(
        &settings,
        Some(&track_id),
        Some(&server_lyrics)
    ));
    assert!(auto_lyrics_skip_action_enabled(
        &settings,
        Some(&track_id),
        Some(&remote_lyrics)
    ));
}
#[test]
pub(in crate::ui) fn auto_lyrics_request_keeps_server_lookup_when_external_search_is_suppressed() {
    let track_id = TrackId::fake(12);
    let mut settings = AppSettings {
        external_lyrics_enabled: true,
        ..AppSettings::default()
    };

    assert_eq!(
        auto_lyrics_request_for_settings(&settings, &track_id, true),
        Some(AutoLyricsRequest::Default)
    );

    settings
        .suppressed_auto_lyrics_track_ids
        .push(track_id.as_str().to_string());
    assert_eq!(
        auto_lyrics_request_for_settings(&settings, &track_id, true),
        Some(AutoLyricsRequest::ServerOnly)
    );

    settings.suppressed_auto_lyrics_track_ids.clear();
    settings.external_lyrics_enabled = false;
    assert_eq!(
        auto_lyrics_request_for_settings(&settings, &track_id, true),
        Some(AutoLyricsRequest::ServerOnly)
    );

    settings.external_lyrics_enabled = true;
    settings.private_mode = true;
    assert_eq!(
        auto_lyrics_request_for_settings(&settings, &track_id, true),
        Some(AutoLyricsRequest::ServerOnly)
    );

    settings.private_mode = false;
    settings.lyrics_panel_visible = false;
    assert_eq!(
        auto_lyrics_request_for_settings(&settings, &track_id, false),
        None
    );
    assert_eq!(
        auto_lyrics_request_for_settings(&settings, &track_id, true),
        Some(AutoLyricsRequest::Default)
    );
}
#[test]
pub(in crate::ui) fn cover_draw_rect_crops_portrait_images_to_square_targets() {
    let rect = super::cover_draw_rect(100, 200, 34, 34);
    assert!((rect.scale - 0.34).abs() < f64::EPSILON);
    assert!((rect.x - 0.0).abs() < f64::EPSILON);
    assert!((rect.y + 17.0).abs() < f64::EPSILON);
}
#[test]
pub(in crate::ui) fn cover_draw_rect_crops_landscape_images_to_square_targets() {
    let rect = super::cover_draw_rect(200, 100, 44, 44);
    assert!((rect.scale - 0.44).abs() < f64::EPSILON);
    assert!((rect.x + 22.0).abs() < f64::EPSILON);
    assert!((rect.y - 0.0).abs() < f64::EPSILON);
}
pub(in crate::ui) fn test_album(artist: &str, artist_id: Option<ArtistId>) -> Album {
    Album {
        id: AlbumId::fake(1),
        title: "Album".to_string(),
        artist: artist.to_string(),
        artist_id,
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
        color_seed: 1,
        image_ref: None,
        genres: Vec::new(),
    }
}
pub(in crate::ui) fn test_home_album_section(kind: HomeSectionKind, album_id: u32) -> HomeSection {
    let mut album = test_album("Album Artist", Some(ArtistId::fake(album_id)));
    album.id = AlbumId::fake(album_id);
    HomeSection {
        kind,
        albums: vec![album],
        tracks: Vec::new(),
    }
}
pub(in crate::ui) fn test_track(artist: &str, artist_id: Option<ArtistId>) -> Track {
    Track {
        id: TrackId::fake(1),
        album_id: AlbumId::fake(1),
        title: "Track".to_string(),
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
        track_number: 1,
        image_ref: None,
        genres: Vec::new(),
        local_path: None,
        source_format: None,
    }
}
