//! Rufin's GTK interface.
//!
//! `shell` owns the window, navigation, and layout; `routes` owns individual
//! pages. Library, playback, and source behavior stays in their own crates.

mod application;
mod downloads;
mod favorites;
mod interactions;
mod layout;
mod localization;
pub(crate) mod player;
mod preferences;
mod ratings;
mod routes;
pub mod runtime;
pub mod settings;
mod shell;

pub use application::{run_application, run_application_after_update};

pub fn verify_interface_resources() -> Result<(), String> {
    application::verify_interface_resources()
}

pub use settings::{
    AccentPreference, DEFAULT_LEFT_SIDEBAR_WIDTH, DEFAULT_RIGHT_SIDEBAR_WIDTH,
    DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, DownloadRule, DownloadRules,
    ExternalSiteLinkSettings, FolderViewSettings, HomeBlockKind, HomeSectionKind, LayoutProfile,
    LayoutSettings, LeftSidebarMode, LibraryField, LibraryLayout, LibraryListKey,
    LibraryListSettings, LibraryListSettingsEntry, MAX_LEFT_SIDEBAR_WIDTH,
    MAX_NARROW_LAYOUT_THRESHOLD, MAX_RIGHT_SIDEBAR_WIDTH, MAX_TABLE_COLUMN_WIDTH,
    MIN_LEFT_SIDEBAR_WIDTH, MIN_NARROW_LAYOUT_THRESHOLD, MIN_RIGHT_SIDEBAR_WIDTH,
    MIN_TABLE_COLUMN_WIDTH, RightSidebarMode, Settings, SettingsHandle, SettingsPort, SidebarPin,
    SidebarRouteItem, SidebarRouteItemSettings, SidebarSettings, ThemePreference,
    available_detail_track_fields, available_grid_fields, available_row_fields,
    available_sort_fields, sanitized_window_size,
};

pub fn format_duration(seconds: u32) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    format!("{minutes}:{seconds:02}")
}

pub(crate) fn format_duration_units(seconds: u32) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        return format!("{hours}h {minutes}m {seconds}s");
    }
    if minutes > 0 {
        return format!("{minutes}m {seconds}s");
    }
    format!("{seconds}s")
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_track_duration() {
        assert_eq!(super::format_duration(0), "0:00");
        assert_eq!(super::format_duration(185), "3:05");
        assert_eq!(super::format_duration(3_661), "61:01");
    }

    #[test]
    fn formats_duration_units() {
        assert_eq!(super::format_duration_units(41), "41s");
        assert_eq!(super::format_duration_units(743), "12m 23s");
        assert_eq!(super::format_duration_units(4_421), "1h 13m 41s");
    }
}
