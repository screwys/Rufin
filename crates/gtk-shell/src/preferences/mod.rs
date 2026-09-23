pub use gtk_preferences::preferences::{
    backup, connect, controller, locate_local_folder, present_add_server_preferences_dialog,
    present_downloads_preferences_dialog, present_library_preferences_dialog,
    present_preferences_dialog,
};
pub(crate) mod dialogs;
mod effects;
mod persistence;
pub(crate) mod source;
