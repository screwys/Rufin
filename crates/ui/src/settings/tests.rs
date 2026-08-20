use super::{
    AccentPreference, ContextMenuItem, ContextMenuItemSettings, DownloadRule, DownloadRules,
    LibraryField, LibraryLayout, LibraryListKey, MAX_RESTORED_WINDOW_HEIGHT,
    MAX_RESTORED_WINDOW_WIDTH, RandomPlayGenreSelection, RandomPlaySettings, Settings, SidebarPin,
    SidebarRouteItem, available_detail_track_fields, available_sort_fields, sanitized_window_size,
};
use library::{AlbumId, GenreId, MusicFolderId, PlayedFilter, PlaylistId, SourceId};

#[test]
fn private_mode_blocks_automatic_external_activity_but_keeps_passive_links() {
    let settings = Settings {
        private_mode: true,
        notifications_enabled: true,
        release_notifications_enabled: true,
        ..Settings::default()
    };

    assert!(settings.allows_notifications());
    assert!(settings.shows_external_site_links());
    assert!(!settings.allows_external_metadata_lookup());
}

#[test]
fn automatic_updates_are_opt_in_and_persisted() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("serialize settings");
    legacy
        .as_object_mut()
        .expect("settings object")
        .remove("automatic_updates_enabled");
    let restored = serde_json::from_value::<Settings>(legacy).expect("restore older settings");
    assert!(!restored.automatic_updates_enabled);

    let mut settings = Settings::default();
    for enabled in [true, false, true, false] {
        settings.automatic_updates_enabled = enabled;
        settings = serde_json::from_value::<Settings>(
            serde_json::to_value(settings).expect("serialize automatic update setting"),
        )
        .expect("restore automatic update setting");
        assert_eq!(settings.automatic_updates_enabled, enabled);
    }
}

#[test]
fn close_preferences_migrate_without_retaining_the_tray_only_setting() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("serialize settings");
    let object = legacy.as_object_mut().expect("settings object");
    object.remove("keep_running_after_close");
    object.insert("tray_enabled".to_string(), false.into());
    object.insert("exit_to_tray".to_string(), true.into());

    let mut restored = serde_json::from_value::<Settings>(legacy).expect("restore older settings");
    restored.sanitize();

    assert!(restored.keep_running_after_close);
    assert!(restored.tray_enabled);
    let persisted = serde_json::to_value(restored).expect("serialize migrated settings");
    assert_eq!(persisted["keep_running_after_close"], true);
    assert!(persisted.get("exit_to_tray").is_none());
}

#[test]
fn keep_running_owns_the_tray_preference_invariant() {
    let mut keep_running = Settings {
        tray_enabled: false,
        keep_running_after_close: true,
        ..Settings::default()
    };
    keep_running.sanitize();
    assert!(keep_running.tray_enabled);

    let mut tray_disabled = Settings {
        tray_enabled: false,
        keep_running_after_close: false,
        start_minimized: true,
        ..Settings::default()
    };
    tray_disabled.sanitize();
    assert!(!tray_disabled.start_minimized);
}

#[test]
fn waveform_seekbar_is_opt_in_and_persists() {
    assert!(!Settings::default().seekbar_waveform_enabled);

    let mut missing = serde_json::to_value(Settings::default()).expect("serialize settings");
    missing
        .as_object_mut()
        .expect("settings object")
        .remove("seekbar_waveform_enabled");
    let restored = serde_json::from_value::<Settings>(missing).expect("restore older settings");
    assert!(!restored.seekbar_waveform_enabled);

    let mut enabled = serde_json::to_value(Settings::default()).expect("serialize settings");
    enabled["seekbar_waveform_enabled"] = true.into();
    let restored = serde_json::from_value::<Settings>(enabled).expect("restore waveform setting");
    assert!(restored.seekbar_waveform_enabled);
}

#[test]
fn accent_preference_defaults_for_older_settings_and_persists() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("serialize settings");
    legacy
        .as_object_mut()
        .expect("settings object")
        .remove("accent_preference");
    let restored = serde_json::from_value::<Settings>(legacy).expect("restore older settings");
    assert_eq!(restored.accent_preference, AccentPreference::System);

    let mut settings = Settings {
        accent_preference: AccentPreference::Purple,
        ..Settings::default()
    };
    settings = serde_json::from_value::<Settings>(
        serde_json::to_value(settings).expect("serialize accent setting"),
    )
    .expect("restore accent setting");
    assert_eq!(settings.accent_preference, AccentPreference::Purple);
}

#[test]
fn playback_modes_are_one_app_wide_settings_value() {
    let settings = Settings {
        auto_dj_enabled: true,
        shuffle_enabled: true,
        repeat_mode: playback::RepeatMode::All,
        ..Settings::default()
    };
    let value = serde_json::to_value(&settings).expect("serialize playback modes");

    assert_eq!(value["auto_dj_enabled"], true);
    assert_eq!(value["shuffle_enabled"], true);
    assert_eq!(value["repeat_mode"], "All");

    let restored = serde_json::from_value::<Settings>(value).expect("restore playback modes");
    assert!(restored.auto_dj_enabled);
    assert!(restored.shuffle_enabled);
    assert_eq!(restored.repeat_mode, playback::RepeatMode::All);
}

#[test]
fn random_play_settings_round_trip_and_scope_the_genre() {
    let source_id = SourceId::new("source");
    let folder_id = MusicFolderId::new("folder");
    let genre_id = GenreId::new("genre");
    let settings = Settings {
        random_play: RandomPlaySettings {
            limit: 42,
            min_year: Some(1990),
            max_year: Some(2020),
            genre: Some(RandomPlayGenreSelection {
                source_id: source_id.clone(),
                music_folder_id: Some(folder_id.clone()),
                genre_id: genre_id.clone(),
            }),
            played_filter: PlayedFilter::Unplayed,
        },
        ..Settings::default()
    };

    let restored = serde_json::from_value::<Settings>(
        serde_json::to_value(settings).expect("serialize random play settings"),
    )
    .expect("restore random play settings");

    assert_eq!(restored.random_play.limit, 42);
    assert_eq!(restored.random_play.min_year, Some(1990));
    assert_eq!(restored.random_play.max_year, Some(2020));
    assert_eq!(restored.random_play.played_filter, PlayedFilter::Unplayed);
    assert_eq!(
        restored
            .random_play
            .selected_genre_id(&source_id, Some(&folder_id)),
        Some(&genre_id)
    );
    assert_eq!(
        restored
            .random_play
            .selected_genre_id(&SourceId::new("other"), Some(&folder_id)),
        None
    );
}

#[test]
fn sidebar_visualizer_is_opt_in_and_persists() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("serialize settings");
    legacy
        .as_object_mut()
        .expect("settings object")
        .remove("visualizer_panel_visible");
    let restored = serde_json::from_value::<Settings>(legacy).expect("restore older settings");
    assert!(!restored.visualizer_panel_visible);

    let settings = Settings {
        visualizer_panel_visible: true,
        ..Settings::default()
    };
    let restored = serde_json::from_value::<Settings>(
        serde_json::to_value(settings).expect("serialize visualizer setting"),
    )
    .expect("restore visualizer setting");
    assert!(restored.visualizer_panel_visible);
}

#[test]
fn context_menu_settings_migrate_and_restore_custom_order() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("serialize settings");
    legacy
        .as_object_mut()
        .expect("settings object")
        .remove("context_menu");
    let restored = serde_json::from_value::<Settings>(legacy).expect("restore older settings");
    assert_eq!(
        restored
            .context_menu
            .items
            .iter()
            .map(|entry| entry.item)
            .collect::<Vec<_>>(),
        ContextMenuItem::all()
    );
    assert!(
        restored
            .context_menu
            .items
            .iter()
            .all(|entry| entry.visible)
    );

    let mut settings = Settings::default();
    settings.context_menu.items.swap(0, 9);
    settings
        .context_menu
        .items
        .iter_mut()
        .find(|entry| entry.item == ContextMenuItem::PlayRadio)
        .expect("radio setting")
        .visible = false;
    let restored = serde_json::from_value::<Settings>(
        serde_json::to_value(&settings).expect("serialize context menu settings"),
    )
    .expect("restore context menu settings");

    assert_eq!(restored.context_menu, settings.context_menu);
}

#[test]
fn settings_sanitize_repairs_partial_context_menu_settings() {
    let mut settings = Settings::default();
    settings.context_menu.items = vec![
        ContextMenuItemSettings {
            item: ContextMenuItem::Download,
            visible: false,
        },
        ContextMenuItemSettings {
            item: ContextMenuItem::Download,
            visible: true,
        },
    ];

    settings.sanitize();

    assert_eq!(
        settings.context_menu.items.len(),
        ContextMenuItem::all().len()
    );
    assert_eq!(
        settings.context_menu.items[0],
        ContextMenuItemSettings {
            item: ContextMenuItem::Download,
            visible: false,
        }
    );
    let download = settings
        .context_menu
        .items
        .iter()
        .find(|entry| entry.item == ContextMenuItem::Download)
        .expect("download setting");
    assert!(!download.visible);
}

#[test]
fn sidebar_pins_restore_in_order_and_migrate_from_older_settings() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("serialize settings");
    let sidebar = legacy["sidebar"]
        .as_object_mut()
        .expect("sidebar settings object");
    sidebar.remove("pins_visible");
    sidebar.remove("pins");
    sidebar.remove("playlist_pin_imported_sources");
    let restored = serde_json::from_value::<Settings>(legacy).expect("restore older settings");
    assert!(restored.sidebar.pins_visible);
    assert!(restored.sidebar.pins.is_empty());
    assert!(restored.sidebar.playlist_pin_imported_sources.is_empty());

    let source_id = SourceId::new("jellyfin:main");
    let album = SidebarPin::Album {
        source_id: source_id.clone(),
        album_id: AlbumId::new("album"),
    };
    let genre = SidebarPin::Genre {
        source_id: source_id.clone(),
        genre_id: GenreId::new("genre"),
    };
    let playlist = SidebarPin::Playlist {
        source_id,
        playlist_id: PlaylistId::new("playlist"),
    };
    let mut settings = Settings::default();
    assert!(settings.sidebar.set_pinned(album.clone(), true));
    assert!(!settings.sidebar.set_pinned(album.clone(), true));
    assert!(settings.sidebar.set_pinned(genre.clone(), true));
    assert!(settings.sidebar.set_pinned(playlist.clone(), true));
    settings.sidebar.pins.push(album.clone());
    settings.sanitize();
    assert_eq!(
        settings.sidebar.pins,
        [album.clone(), genre.clone(), playlist.clone()]
    );

    let mut restored = serde_json::from_value::<Settings>(
        serde_json::to_value(settings).expect("serialize pinned settings"),
    )
    .expect("restore pinned settings");
    assert_eq!(restored.sidebar.pins, [album.clone(), genre, playlist]);
    assert!(restored.sidebar.set_pinned(album.clone(), false));
    assert!(!restored.sidebar.is_pinned(&album));
}

#[test]
fn remote_playlist_pins_are_imported_once_per_source() {
    let source_id = SourceId::new("jellyfin:main");
    let other_source_id = SourceId::new("subsonic:main");
    let first_playlist_id = PlaylistId::new("first");
    let second_playlist_id = PlaylistId::new("second");
    let later_playlist_id = PlaylistId::new("later");
    let existing_pin = SidebarPin::Album {
        source_id: source_id.clone(),
        album_id: AlbumId::new("album"),
    };
    let first_playlist_pin = SidebarPin::Playlist {
        source_id: source_id.clone(),
        playlist_id: first_playlist_id.clone(),
    };
    let second_playlist_pin = SidebarPin::Playlist {
        source_id: source_id.clone(),
        playlist_id: second_playlist_id.clone(),
    };
    let mut settings = Settings::default();
    assert!(settings.sidebar.set_pinned(existing_pin.clone(), true));

    assert!(settings.sidebar.import_playlist_pins_once(
        source_id.clone(),
        [first_playlist_id.clone(), second_playlist_id]
    ));
    assert_eq!(
        settings.sidebar.pins,
        [
            existing_pin,
            first_playlist_pin.clone(),
            second_playlist_pin
        ]
    );

    assert!(
        settings
            .sidebar
            .set_pinned(first_playlist_pin.clone(), false)
    );
    let mut settings = serde_json::from_value::<Settings>(
        serde_json::to_value(settings).expect("serialize imported playlist Pins"),
    )
    .expect("restore imported playlist Pins");
    assert!(
        !settings
            .sidebar
            .import_playlist_pins_once(source_id, [first_playlist_id.clone(), later_playlist_id])
    );
    assert!(!settings.sidebar.is_pinned(&first_playlist_pin));

    assert!(
        settings
            .sidebar
            .import_playlist_pins_once(other_source_id.clone(), [first_playlist_id.clone()])
    );
    assert!(settings.sidebar.is_pinned(&SidebarPin::Playlist {
        source_id: other_source_id,
        playlist_id: first_playlist_id,
    }));
}

#[test]
fn empty_remote_playlist_import_is_still_complete() {
    let source_id = SourceId::new("jellyfin:empty");
    let mut settings = Settings::default();

    assert!(
        settings
            .sidebar
            .import_playlist_pins_once(source_id.clone(), [])
    );
    assert!(
        !settings
            .sidebar
            .import_playlist_pins_once(source_id, [PlaylistId::new("later")])
    );
    assert!(settings.sidebar.pins.is_empty());
}

#[test]
fn sidebar_pin_identity_includes_the_source() {
    let first = SidebarPin::Album {
        source_id: SourceId::new("jellyfin:first"),
        album_id: AlbumId::new("album"),
    };
    let second = SidebarPin::Album {
        source_id: SourceId::new("jellyfin:second"),
        album_id: AlbumId::new("album"),
    };
    let mut settings = Settings::default();

    assert!(settings.sidebar.set_pinned(first.clone(), true));
    assert!(settings.sidebar.set_pinned(second.clone(), true));
    assert_eq!(settings.sidebar.pins, [first, second]);
}

#[test]
fn download_rules_are_saved_for_one_source() {
    let mut settings = Settings::default();
    let first = SourceId::new("jellyfin:first");
    let second = SourceId::new("subsonic:second");
    let rules = DownloadRules {
        favorites: true,
        latest_five_albums: true,
        ..DownloadRules::default()
    };

    assert!(settings.set_download_rules(first.clone(), rules));
    assert_eq!(settings.download_rules(&first), rules);
    assert_eq!(settings.download_rules(&second), DownloadRules::default());

    let restored = serde_json::from_value::<Settings>(
        serde_json::to_value(settings).expect("serialize download rules"),
    )
    .expect("restore download rules");
    assert_eq!(restored.download_rules(&first), rules);
}

#[test]
fn downloaded_badges_are_shown_by_default_and_can_be_hidden() {
    let mut legacy = serde_json::to_value(Settings::default()).expect("serialize settings");
    legacy
        .as_object_mut()
        .expect("settings object")
        .remove("show_downloaded_badges");
    let restored = serde_json::from_value::<Settings>(legacy).expect("restore older settings");
    assert!(restored.show_downloaded_badges);

    let hidden = Settings {
        show_downloaded_badges: false,
        ..Settings::default()
    };
    let restored = serde_json::from_value::<Settings>(
        serde_json::to_value(hidden).expect("serialize downloaded badge setting"),
    )
    .expect("restore downloaded badge setting");
    assert!(!restored.show_downloaded_badges);
}

#[test]
fn download_rules_are_independent_addable_entries() {
    let mut rules = DownloadRules::default();
    rules.set(DownloadRule::AllPlaylists, true);
    rules.set(DownloadRule::LatestFiveAlbums, true);

    assert_eq!(
        rules.active().collect::<Vec<_>>(),
        vec![DownloadRule::AllPlaylists, DownloadRule::LatestFiveAlbums]
    );

    rules.set(DownloadRule::AllPlaylists, false);
    assert!(!rules.contains(DownloadRule::AllPlaylists));
    assert!(rules.contains(DownloadRule::LatestFiveAlbums));
}

#[test]
fn original_download_quality_is_the_source_default() {
    let mut settings = Settings::default();
    let source_id = SourceId::new("jellyfin:quality");

    assert_eq!(
        settings.download_quality(&source_id),
        library::StreamQuality::Original
    );
    assert!(settings.set_download_quality(
        source_id.clone(),
        library::StreamQuality::MaxBitrateKbps(192)
    ));
    assert_eq!(
        settings.download_quality(&source_id),
        library::StreamQuality::MaxBitrateKbps(192)
    );
}

#[test]
fn download_folder_is_optional_and_source_scoped() {
    let mut settings = Settings::default();
    let first = SourceId::new("jellyfin:folder");
    let second = SourceId::new("subsonic:folder");
    let directory = std::path::PathBuf::from("/music/offline");

    assert_eq!(settings.download_directory(&first), None);
    assert!(settings.set_download_directory(first.clone(), Some(directory.clone())));
    assert_eq!(settings.download_directory(&first), Some(directory.clone()));
    assert_eq!(settings.download_directory(&second), None);

    let restored = serde_json::from_value::<Settings>(
        serde_json::to_value(settings).expect("serialize download folder"),
    )
    .expect("restore download folder");
    assert_eq!(restored.download_directory(&first), Some(directory));
}

#[test]
fn split_lyrics_and_metadata_settings_keep_the_released_flat_keys() {
    let value = serde_json::to_value(Settings::default()).expect("serialize settings");

    assert_eq!(value["external_metadata_enabled"], true);
    assert!(value.get("external_album_lookup_enabled").is_none());
    assert!(value.get("lyrics").is_none());
    assert!(value.get("metadata").is_none());

    let mut disabled = value.clone();
    disabled["external_metadata_enabled"] = false.into();
    let disabled =
        serde_json::from_value::<Settings>(disabled).expect("deserialize released setting");
    assert!(!disabled.external_metadata_enabled);

    let mut missing = value;
    missing
        .as_object_mut()
        .expect("settings object")
        .remove("external_metadata_enabled");
    let missing = serde_json::from_value::<Settings>(missing).expect("deserialize sparse settings");
    assert!(missing.external_metadata_enabled);
}

#[test]
fn restored_window_size_is_bounded() {
    assert_eq!(sanitized_window_size(None, Some(700)), None);
    assert_eq!(sanitized_window_size(Some(400), Some(700)), None);
    assert_eq!(
        sanitized_window_size(Some(1061), Some(2251)),
        Some((1061, MAX_RESTORED_WINDOW_HEIGHT))
    );
    assert_eq!(
        sanitized_window_size(Some(1800), Some(1200)),
        Some((1800, 1200))
    );
    assert_eq!(
        sanitized_window_size(Some(5000), Some(3000)),
        Some((MAX_RESTORED_WINDOW_WIDTH, MAX_RESTORED_WINDOW_HEIGHT))
    );
}

#[test]
fn unknown_library_layout_falls_back_to_grid() {
    let layout =
        serde_json::from_str::<LibraryLayout>("\"weird\"").expect("deserialize library layout");
    assert_eq!(layout, LibraryLayout::Grid);
}

#[test]
fn saved_table_widths_round_trip_with_safe_bounds() {
    let mut settings = Settings::default();
    let tracks = settings
        .library_lists
        .iter_mut()
        .find(|entry| entry.key == LibraryListKey::Tracks)
        .expect("default Tracks settings");
    tracks.settings.row_column_widths = vec![
        super::LibraryColumnWidth {
            field: LibraryField::Title,
            width: 12,
        },
        super::LibraryColumnWidth {
            field: LibraryField::Title,
            width: 900,
        },
        super::LibraryColumnWidth {
            field: LibraryField::RowIndex,
            width: 80,
        },
    ];
    settings.folder_view.tree_width = Some(10_000);
    settings.folder_view.name_column_width = Some(12);
    settings.folder_view.detail_column_width = Some(10_000);
    settings.sanitize();

    let value = serde_json::to_value(&settings).expect("serialize saved table widths");
    let restored =
        serde_json::from_value::<Settings>(value).expect("deserialize saved table widths");
    let tracks = restored.library_list(LibraryListKey::Tracks);

    assert_eq!(
        tracks.row_column_widths,
        [super::LibraryColumnWidth {
            field: LibraryField::Title,
            width: super::MIN_TABLE_COLUMN_WIDTH,
        }]
    );
    assert_eq!(
        restored.folder_view.tree_width,
        Some(super::MAX_TABLE_COLUMN_WIDTH)
    );
    assert_eq!(
        restored.folder_view.name_column_width,
        Some(super::MIN_TABLE_COLUMN_WIDTH)
    );
    assert_eq!(
        restored.folder_view.detail_column_width,
        Some(super::MAX_TABLE_COLUMN_WIDTH)
    );
}

#[test]
fn default_library_rows_skip_redundant_album_artist() {
    for key in [LibraryListKey::Albums, LibraryListKey::ArtistAlbums] {
        let settings = super::LibraryListSettings::for_key(key);
        assert_eq!(
            settings.row_fields,
            vec![
                LibraryField::TitleMerged,
                LibraryField::PlayCount,
                LibraryField::Year,
                LibraryField::Favorite,
            ],
            "{key:?}"
        );
    }
}

#[test]
fn default_artist_tracks_use_normal_track_rows() {
    let tracks = super::LibraryListSettings::for_key(LibraryListKey::Tracks);
    assert_eq!(
        tracks.row_fields,
        vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Favorite,
        ]
    );

    for key in [LibraryListKey::FavoriteTracks, LibraryListKey::ArtistTracks] {
        let settings = super::LibraryListSettings::for_key(key);
        assert_eq!(
            settings.row_fields,
            vec![
                LibraryField::RowIndex,
                LibraryField::TitleMerged,
                LibraryField::Album,
                LibraryField::Year,
                LibraryField::PlayCount,
            ],
            "{key:?}"
        );
    }
}

#[test]
fn history_is_an_enabled_occurrence_route_by_default() {
    let mut settings = Settings::default();
    let history = settings.library_list(LibraryListKey::History);
    assert_eq!(history.layout, LibraryLayout::Row);
    assert_eq!(history.sort_key, LibraryField::LastPlayed);
    assert!(history.descending);
    assert_eq!(history.row_fields.first(), Some(&LibraryField::RowIndex));
    assert!(history.row_fields.contains(&LibraryField::LastPlayed));
    assert_eq!(
        available_sort_fields(LibraryListKey::History),
        &[LibraryField::LastPlayed]
    );
    assert!(
        settings
            .sidebar
            .route_items
            .iter()
            .any(|entry| entry.item == SidebarRouteItem::History && entry.visible)
    );
    let history_position = settings
        .sidebar
        .route_items
        .iter()
        .position(|entry| entry.item == SidebarRouteItem::History)
        .expect("History is present");
    assert_eq!(
        settings.sidebar.route_items[history_position - 1].item,
        SidebarRouteItem::Moods
    );
    assert_eq!(
        settings.sidebar.route_items[history_position + 1].item,
        SidebarRouteItem::Folders
    );

    settings
        .sidebar
        .route_items
        .retain(|entry| entry.item != SidebarRouteItem::History);
    settings.sanitize();
    let history_position = settings
        .sidebar
        .route_items
        .iter()
        .position(|entry| entry.item == SidebarRouteItem::History)
        .expect("sanitize restores History");
    assert_eq!(
        settings
            .sidebar
            .route_items
            .get(history_position)
            .map(|entry| entry.visible),
        Some(true)
    );
    assert_eq!(
        settings.sidebar.route_items[history_position - 1].item,
        SidebarRouteItem::Moods
    );
    assert_eq!(
        settings.sidebar.route_items[history_position + 1].item,
        SidebarRouteItem::Folders
    );
}

#[test]
fn search_is_available_in_the_sidebar_but_hidden_by_default() {
    let settings = Settings::default();
    let search = settings
        .sidebar
        .route_items
        .iter()
        .find(|entry| entry.item == SidebarRouteItem::Search)
        .expect("Search is available");
    assert!(!search.visible);
    assert_eq!(settings.sidebar.route_items[0].item, SidebarRouteItem::Home);
    assert_eq!(
        settings.sidebar.route_items[1].item,
        SidebarRouteItem::Search
    );
}

#[test]
fn default_albums_use_grid() {
    let settings = super::LibraryListSettings::for_key(LibraryListKey::Albums);
    assert_eq!(settings.layout, LibraryLayout::Grid);
}

#[test]
fn playlist_track_sorting_stays_within_playlist_playback_ordering() {
    let settings = super::LibraryListSettings::for_key(LibraryListKey::PlaylistTracks);
    assert_eq!(settings.sort_key, LibraryField::RowIndex);
    assert_eq!(
        available_sort_fields(LibraryListKey::PlaylistTracks),
        &[
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Artist,
            LibraryField::Album,
        ]
    );
}

#[test]
fn track_row_defaults_start_with_the_index() {
    for key in [
        LibraryListKey::Tracks,
        LibraryListKey::FavoriteTracks,
        LibraryListKey::History,
        LibraryListKey::AlbumDetailTracks,
        LibraryListKey::ArtistTracks,
        LibraryListKey::GenreTracks,
        LibraryListKey::MoodTracks,
        LibraryListKey::PlaylistTracks,
        LibraryListKey::SmartPlaylistTracks,
    ] {
        assert_eq!(
            super::LibraryListSettings::for_key(key).row_fields.first(),
            Some(&LibraryField::RowIndex),
            "{key:?}"
        );
    }

    assert_eq!(
        available_detail_track_fields(),
        &[
            LibraryField::RowIndex,
            LibraryField::TrackNumber,
            LibraryField::Title,
            LibraryField::Duration,
        ]
    );
    assert_eq!(
        super::LibraryListSettings::for_key(LibraryListKey::Albums).detail_track_fields,
        [
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Duration,
        ]
    );
}

#[test]
fn library_list_settings_migrate_persisted_layout_versions() {
    let mut playlists = super::LibraryListSettings {
        layout: LibraryLayout::Grid,
        row_fields: vec![
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount,
            LibraryField::Duration,
        ],
        grid_fields: vec![LibraryField::SongCount, LibraryField::Duration],
        detail_track_fields: Vec::new(),
        sort_key: LibraryField::Title,
        descending: false,
        row_column_widths: Vec::new(),
        layout_version: 2,
    };
    playlists.sanitize(LibraryListKey::Playlists);
    assert_eq!(
        playlists.row_fields,
        vec![
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::SongCount
        ]
    );
    assert_eq!(playlists.grid_fields, vec![LibraryField::SongCount]);

    let mut smart_playlists = super::LibraryListSettings {
        layout: LibraryLayout::Grid,
        row_fields: vec![LibraryField::Image, LibraryField::Title],
        grid_fields: vec![LibraryField::SongCount],
        detail_track_fields: Vec::new(),
        sort_key: LibraryField::Title,
        descending: false,
        row_column_widths: Vec::new(),
        layout_version: 3,
    };
    smart_playlists.sanitize(LibraryListKey::SmartPlaylists);
    assert_eq!(smart_playlists.sort_key, LibraryField::RowIndex);

    let mut albums = super::LibraryListSettings {
        layout: LibraryLayout::Detail,
        row_fields: vec![LibraryField::Image, LibraryField::Title],
        grid_fields: vec![LibraryField::AlbumArtist],
        detail_track_fields: vec![
            LibraryField::RowIndex,
            LibraryField::Image,
            LibraryField::Title,
            LibraryField::PlayCount,
        ],
        sort_key: LibraryField::Title,
        descending: false,
        row_column_widths: Vec::new(),
        layout_version: 4,
    };
    albums.sanitize(LibraryListKey::Albums);
    assert_eq!(
        albums.detail_track_fields,
        [
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Duration,
        ]
    );

    let mut favorite_tracks = super::LibraryListSettings {
        layout: LibraryLayout::Row,
        row_fields: vec![
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Favorite,
        ],
        grid_fields: Vec::new(),
        detail_track_fields: available_detail_track_fields().to_vec(),
        sort_key: LibraryField::Title,
        descending: false,
        row_column_widths: Vec::new(),
        layout_version: 6,
    };
    favorite_tracks.sanitize(LibraryListKey::FavoriteTracks);
    assert_eq!(
        favorite_tracks.row_fields,
        vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::PlayCount,
        ]
    );

    let mut tracks = super::LibraryListSettings {
        layout: LibraryLayout::Row,
        row_fields: vec![
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Year,
            LibraryField::Favorite,
        ],
        grid_fields: Vec::new(),
        detail_track_fields: vec![
            LibraryField::TrackNumber,
            LibraryField::Title,
            LibraryField::Duration,
        ],
        sort_key: LibraryField::Title,
        descending: false,
        row_column_widths: Vec::new(),
        layout_version: 7,
    };
    tracks.sanitize(LibraryListKey::Tracks);
    assert_eq!(tracks.row_fields.first(), Some(&LibraryField::RowIndex));
    assert_eq!(
        tracks.detail_track_fields,
        [
            LibraryField::RowIndex,
            LibraryField::Title,
            LibraryField::Duration,
        ]
    );

    let mut custom_tracks = super::LibraryListSettings {
        row_fields: vec![LibraryField::Image, LibraryField::TitleMerged],
        layout_version: 7,
        ..super::LibraryListSettings::for_key(LibraryListKey::Tracks)
    };
    custom_tracks.sanitize(LibraryListKey::Tracks);
    assert_eq!(
        custom_tracks.row_fields,
        [LibraryField::Image, LibraryField::TitleMerged]
    );

    let mut history = super::LibraryListSettings {
        layout: LibraryLayout::Row,
        row_fields: vec![
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::Duration,
            LibraryField::Favorite,
        ],
        grid_fields: vec![
            LibraryField::Artist,
            LibraryField::Album,
            LibraryField::Duration,
        ],
        detail_track_fields: available_detail_track_fields().to_vec(),
        sort_key: LibraryField::RowIndex,
        descending: false,
        row_column_widths: Vec::new(),
        layout_version: 8,
    };
    history.sanitize(LibraryListKey::History);
    assert_eq!(history.sort_key, LibraryField::LastPlayed);
    assert!(history.descending);
    assert_eq!(
        history.row_fields,
        [
            LibraryField::RowIndex,
            LibraryField::TitleMerged,
            LibraryField::Album,
            LibraryField::LastPlayed,
            LibraryField::Favorite,
        ]
    );
    assert_eq!(
        history.grid_fields,
        [
            LibraryField::Artist,
            LibraryField::Album,
            LibraryField::LastPlayed,
        ]
    );
}
