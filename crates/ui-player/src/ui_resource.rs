pub const LYRICS_PANE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/pane.ui";
pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    PLAYBACK_SETTINGS_POPOVER_RESOURCE,
    LYRICS_COLOR_CHOOSER_RESOURCE,
    LYRICS_SETTINGS_RESOURCE,
    QUEUE_FULLSCREEN_ROW_RESOURCE,
    QUEUE_SIDEBAR_ROW_RESOURCE,
    RIGHT_PANEL_RESOURCE,
    LYRICS_SEARCH_RESOURCE,
    LYRICS_EDIT_RESOURCE,
    LYRICS_DICTIONARY_RESOURCE,
    EQUALIZER_RESOURCE,
    FULLSCREEN_PLAYER_RESOURCE,
    BOTTOM_PLAYER_RESOURCE,
    LYRICS_PANE_RESOURCE,
];

pub const BOTTOM_PLAYER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/bottom.ui";

pub const FULLSCREEN_PLAYER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/fullscreen.ui";

pub const EQUALIZER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/equalizer.ui";

pub const LYRICS_DICTIONARY_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/lyrics/dictionary.ui";

pub const LYRICS_EDIT_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/edit.ui";

pub const LYRICS_SEARCH_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/search.ui";

pub const RIGHT_PANEL_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/right_panel.ui";

pub const QUEUE_SIDEBAR_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/queue_sidebar_row.ui";

pub const QUEUE_FULLSCREEN_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/queue_fullscreen_row.ui";

pub const LYRICS_SETTINGS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/settings.ui";

pub const LYRICS_COLOR_CHOOSER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/lyrics/color_chooser.ui";

pub const PLAYBACK_SETTINGS_POPOVER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/playback_settings.ui";

#[cfg(test)]
mod resource_tests {
    #[test]
    fn owned_interface_resources_are_compiled() {
        crate::register_resources().expect("resource registration");
        for path in super::INTERFACE_RESOURCE_PATHS {
            gtk::gio::resources_lookup_data(path, gtk::gio::ResourceLookupFlags::NONE)
                .expect("compiled interface resource");
        }
    }
}
