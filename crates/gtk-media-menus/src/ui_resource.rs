pub use gtk_widgets::ui_resource::{builder, object};
pub const PLAYLIST_PICKER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/playlist_picker.ui";
pub const PLAYLIST_PICKER_CONTEXT_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/playlist_picker_context.ui";
pub const PLAYLIST_PICKER_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/playlist_picker_row.ui";

pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    PLAYLIST_PICKER_RESOURCE,
    PLAYLIST_PICKER_CONTEXT_RESOURCE,
    PLAYLIST_PICKER_ROW_RESOURCE,
];
