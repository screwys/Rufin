pub const METADATA_LOOKUP_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_lookup.ui";
pub const METADATA_LYRICS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_lyrics.ui";
pub const METADATA_FIELD_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_field.ui";
pub const METADATA_TRACK_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_track.ui";
pub const METADATA_BATCH_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata_batch.ui";
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
    METADATA_LYRICS_RESOURCE,
    METADATA_LOOKUP_RESOURCE,
    METADATA_DIALOG_RESOURCE,
    METADATA_BATCH_RESOURCE,
    METADATA_FIELD_RESOURCE,
    SMART_PLAYLIST_DIALOG_RESOURCE,
    PLAYLIST_ARTWORK_RESOURCE,
    METADATA_ARTWORK_RESOURCE,
    METADATA_ARTWORK_RESULT_RESOURCE,
];

pub const TEMPLATE_RESOURCE_PATHS: &[&str] = &[METADATA_TRACK_RESOURCE];
