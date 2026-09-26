pub use gtk_widgets::ui_resource::{builder, object};
pub const METADATA_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata.ui";
pub const SMART_PLAYLIST_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/smart_playlist.ui";
pub const METADATA_ARTWORK_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_artwork.ui";
pub const METADATA_ARTWORK_RESULT_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_artwork_result.ui";

pub const PLAYLIST_ARTWORK_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/playlist_artwork.ui";

pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    METADATA_DIALOG_RESOURCE,
    SMART_PLAYLIST_DIALOG_RESOURCE,
    PLAYLIST_ARTWORK_RESOURCE,
    METADATA_ARTWORK_RESOURCE,
    METADATA_ARTWORK_RESULT_RESOURCE,
];
