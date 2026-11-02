//! The GTK desktop client: window composition, pages, player, menus, editors and
//! preferences.

mod downloads;
mod editors;
mod favorites;
mod library;
mod menus;
mod player;
mod playlists;
mod preferences;
mod ratings;
mod shell;
mod ui_resource;
mod widgets;

pub use shell::{
    ApplicationExit, run_application, run_application_after_update, run_startup_error_application,
};

/// Registers the interface definitions, styles and icons compiled into the binary.
pub fn register_resources() -> Result<(), String> {
    use gtk::glib::types::StaticType;
    widgets::edge_fade::TextFade::static_type();
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gtk::gio::resources_register_include!("rufin.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}
