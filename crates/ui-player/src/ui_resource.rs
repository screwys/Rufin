pub const LYRICS_PANE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/pane.ui";
pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    VISUALIZER_SETTINGS_RESOURCE,
    VISUALIZER_PASTE_RESOURCE,
    PLAYBACK_SETTINGS_POPOVER_RESOURCE,
    COLOR_CHOOSER_RESOURCE,
    LYRICS_SETTINGS_RESOURCE,
    RIGHT_PANEL_RESOURCE,
    QUEUE_SETTINGS_RESOURCE,
    SIDEBAR_CONTROLS_RESOURCE,
    SIDEBAR_SPLIT_RESOURCE,
    LYRICS_SEARCH_RESOURCE,
    LYRICS_EDIT_RESOURCE,
    LYRICS_DICTIONARY_RESOURCE,
    EQUALIZER_RESOURCE,
    FULLSCREEN_PLAYER_RESOURCE,
    FULLSCREEN_SETTINGS_RESOURCE,
    BOTTOM_PLAYER_RESOURCE,
    LYRICS_PANE_RESOURCE,
];
pub const VISUALIZER_SETTINGS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/visualizer_settings.ui";
pub const VISUALIZER_PASTE_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/visualizer_paste.ui";

pub const BOTTOM_PLAYER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/bottom.ui";

pub const FULLSCREEN_PLAYER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/fullscreen.ui";
pub const FULLSCREEN_SETTINGS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/fullscreen_settings.ui";

pub const EQUALIZER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/equalizer.ui";

pub const LYRICS_DICTIONARY_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/lyrics/dictionary.ui";

pub const LYRICS_EDIT_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/edit.ui";

pub const LYRICS_SEARCH_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/search.ui";

pub const RIGHT_PANEL_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/right_panel.ui";
pub const QUEUE_SETTINGS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/queue_settings.ui";
pub const SIDEBAR_CONTROLS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/sidebar_controls.ui";
pub const SIDEBAR_SPLIT_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/sidebar_split.ui";

pub const LYRICS_SETTINGS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/settings.ui";

pub const COLOR_CHOOSER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/color_chooser.ui";

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
