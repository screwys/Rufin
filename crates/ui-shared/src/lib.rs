//! Shared GTK presentation owners and primitives.
pub mod field_layout;
pub mod layout;
pub mod localization;
pub mod route;
pub mod settings;
pub mod sparse_model;

pub mod artwork;

pub mod downloads;

pub mod ui_resource;

pub fn register_resources() -> Result<(), String> {
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gtk::gio::resources_register_include!("ui-shared.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}

#[cfg(test)]
mod cell_lifetime_tests;
pub mod detail_links;
pub mod folder_launcher;
pub mod recycled_cells;
pub mod source_labels;

pub mod controls;
pub mod interactions;
pub mod ratings;

pub mod metadata;

pub mod playlists;
pub mod popup;

pub mod mounted_route;

pub mod selection;
pub mod smart_playlist;

pub mod favorites;

pub mod local_access;

mod duration;
pub mod library_fields;
pub mod table_sizing;
pub use duration::{format_duration, format_duration_units};

pub mod scale;

pub mod feedback;

pub mod media_drag;

pub mod forms;

pub mod library_field_editor;

pub mod cover_controls;

pub mod playlist_picker;

pub mod media_menus;
