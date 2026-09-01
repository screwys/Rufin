use gtk::glib;
use gtk::prelude::*;

macro_rules! composite_box {
    ($vis:vis $name:ident, $imp:ident, $gtype:literal, $resource:literal, {
        $($field:ident: $type:ty),+ $(,)?
    }) => {
        pub(crate) mod $imp {
            use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};

            #[derive(CompositeTemplate, Default)]
            #[template(resource = $resource)]
            pub(crate) struct $name {
                $(
                    #[template_child]
                    pub(crate) $field: TemplateChild<$type>,
                )+
            }

            #[glib::object_subclass]
            impl ObjectSubclass for $name {
                const NAME: &'static str = $gtype;
                type Type = super::$name;
                type ParentType = gtk::Box;

                fn class_init(class: &mut Self::Class) {
                    class.bind_template();
                }

                fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
                    instance.init_template();
                }
            }

            impl ObjectImpl for $name {}
            impl WidgetImpl for $name {}
            impl BoxImpl for $name {}
        }

        gtk::glib::wrapper! {
            $vis struct $name(ObjectSubclass<$imp::$name>)
                @extends gtk::Widget, gtk::Box,
                @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
        }

        impl $name {
            fn new() -> Self {
                gtk::glib::Object::new()
            }
        }
    };
}

pub(crate) use composite_box;

macro_rules! objects {
    ($builder:expr, $resource:expr, { $($name:ident: $type:ty),+ $(,)? }) => {
        $(
            let $name: $type =
                crate::ui_resource::object(&$builder, $resource, stringify!($name));
        )+
    };
}

pub(crate) use objects;

pub(crate) const BASE_CSS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/style.css";
pub(crate) const ABOUT_RESOURCE: &str = "/io/github/screwys/Rufin/ui/application/about.ui";
pub(crate) const RANDOM_PLAY_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/random_play.ui";
pub(crate) const BOTTOM_PLAYER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/bottom.ui";
pub(crate) const EQUALIZER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/equalizer.ui";
pub(crate) const FULLSCREEN_PLAYER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/fullscreen.ui";
pub(crate) const PLAYBACK_SETTINGS_POPOVER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/playback_settings.ui";
pub(crate) const RIGHT_PANEL_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/right_panel.ui";
pub(crate) const QUEUE_SIDEBAR_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/queue_sidebar_row.ui";
pub(crate) const QUEUE_FULLSCREEN_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/queue_fullscreen_row.ui";
pub(crate) const LYRICS_COLOR_CHOOSER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/lyrics/color_chooser.ui";
pub(crate) const LYRICS_EDIT_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/edit.ui";
pub(crate) const LYRICS_PANE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/player/lyrics/pane.ui";
pub(crate) const LYRICS_SEARCH_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/lyrics/search.ui";
pub(crate) const LYRICS_SETTINGS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/player/lyrics/settings.ui";
pub(crate) const APPEARANCE_PREFERENCES_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/appearance.ui";
pub(crate) const PREFERENCES_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialog.ui";
pub(crate) const GENERAL_PREFERENCES_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/general.ui";
pub(crate) const INTEGRATIONS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/integrations.ui";
pub(crate) const LIBRARY_PREFERENCES_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/library.ui";
pub(crate) const METADATA_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata.ui";
pub(crate) const PLAYLIST_NAME_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/playlist_name.ui";
pub(crate) const RELEASE_NOTE_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/release_note_row.ui";
pub(crate) const RELEASE_NOTES_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/release_notes.ui";
pub(crate) const SMART_PLAYLIST_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/smart_playlist.ui";
pub(crate) const PLAYBACK_PREFERENCES_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/playback.ui";
pub(crate) const PREFERENCES_REORDER_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/reorder_row.ui";
pub(crate) const CONNECTION_PROGRESS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/connection_progress.ui";
pub(crate) const SOURCE_CHOICE_SELECTOR_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/choice_selector.ui";
pub(crate) const CREDENTIAL_HOST_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/credential_host.ui";
pub(crate) const LOCAL_SETUP_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/local_setup.ui";
pub(crate) const MANAGE_SERVER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/manage_server.ui";
pub(crate) const METADATA_RECOVERY_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/metadata_recovery.ui";
pub(crate) const SERVER_ACTIONS_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/server_actions.ui";
pub(crate) const SETUP_SCAFFOLD_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/setup_scaffold.ui";
pub(crate) const COLLECTION_GRID_CARD_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/collection_grid_card.ui";
pub(crate) const COLLECTION_GRID_COVER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/collection_grid_cover.ui";
pub(crate) const DETAIL_SHOWCASE_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/detail_showcase.ui";
pub(crate) const FOLDERS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/folders.ui";
pub(crate) const HOME_SECTION_HEADER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/home_section_header.ui";
pub(crate) const LIBRARY_CONFIG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/library_config.ui";
pub(crate) const LIBRARY_PAGE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/library_page.ui";
pub(crate) const LIBRARY_TOOLBAR_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/library_toolbar.ui";
pub(crate) const ROUTE_PLACEHOLDER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/placeholder.ui";
pub(crate) const PLAYLIST_PICKER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/playlist_picker.ui";
pub(crate) const PLAYLIST_PICKER_CONTEXT_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/playlist_picker_context.ui";
pub(crate) const PLAYLIST_PICKER_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/playlist_picker_row.ui";
pub(crate) const RECYCLED_ARTWORK_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_artwork_cell.ui";
pub(crate) const RECYCLED_BADGED_TEXT_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_badged_text_cell.ui";
pub(crate) const RECYCLED_FOLDER_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_folder_cell.ui";
pub(crate) const RECYCLED_MERGED_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_merged_cell.ui";
pub(crate) const RECYCLED_TEXT_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_text_cell.ui";
pub(crate) const SEARCH_ROUTE_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/search.ui";
pub(crate) const DIAGNOSTICS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/shell/diagnostics.ui";
pub(crate) const CONTENT_CHROME_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/shell/content_chrome.ui";
pub(crate) const NAVIGATION_RESOURCE: &str = "/io/github/screwys/Rufin/ui/shell/navigation.ui";
pub(crate) const SHELL_ROOT_RESOURCE: &str = "/io/github/screwys/Rufin/ui/shell/root.ui";
pub(crate) const SHORTCUTS_RESOURCE: &str = "/io/github/screwys/Rufin/ui/shell/shortcuts.ui";
pub(crate) const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    BASE_CSS_RESOURCE,
    ABOUT_RESOURCE,
    BOTTOM_PLAYER_RESOURCE,
    EQUALIZER_RESOURCE,
    FULLSCREEN_PLAYER_RESOURCE,
    PLAYBACK_SETTINGS_POPOVER_RESOURCE,
    LYRICS_COLOR_CHOOSER_RESOURCE,
    LYRICS_EDIT_RESOURCE,
    LYRICS_PANE_RESOURCE,
    LYRICS_SEARCH_RESOURCE,
    LYRICS_SETTINGS_RESOURCE,
    RIGHT_PANEL_RESOURCE,
    QUEUE_SIDEBAR_ROW_RESOURCE,
    QUEUE_FULLSCREEN_ROW_RESOURCE,
    APPEARANCE_PREFERENCES_RESOURCE,
    PREFERENCES_DIALOG_RESOURCE,
    GENERAL_PREFERENCES_RESOURCE,
    COLLECTION_GRID_CARD_RESOURCE,
    COLLECTION_GRID_COVER_RESOURCE,
    DETAIL_SHOWCASE_RESOURCE,
    FOLDERS_RESOURCE,
    HOME_SECTION_HEADER_RESOURCE,
    LIBRARY_CONFIG_RESOURCE,
    CONTENT_CHROME_RESOURCE,
    DIAGNOSTICS_RESOURCE,
    INTEGRATIONS_RESOURCE,
    LIBRARY_PREFERENCES_RESOURCE,
    METADATA_DIALOG_RESOURCE,
    PLAYLIST_NAME_DIALOG_RESOURCE,
    RELEASE_NOTE_ROW_RESOURCE,
    RELEASE_NOTES_RESOURCE,
    SMART_PLAYLIST_DIALOG_RESOURCE,
    PLAYBACK_PREFERENCES_RESOURCE,
    PREFERENCES_REORDER_ROW_RESOURCE,
    CONNECTION_PROGRESS_RESOURCE,
    SOURCE_CHOICE_SELECTOR_RESOURCE,
    CREDENTIAL_HOST_RESOURCE,
    LOCAL_SETUP_RESOURCE,
    MANAGE_SERVER_RESOURCE,
    METADATA_RECOVERY_RESOURCE,
    SERVER_ACTIONS_RESOURCE,
    NAVIGATION_RESOURCE,
    SHELL_ROOT_RESOURCE,
    SETUP_SCAFFOLD_RESOURCE,
    LIBRARY_PAGE_RESOURCE,
    LIBRARY_TOOLBAR_RESOURCE,
    ROUTE_PLACEHOLDER_RESOURCE,
    PLAYLIST_PICKER_RESOURCE,
    PLAYLIST_PICKER_CONTEXT_RESOURCE,
    PLAYLIST_PICKER_ROW_RESOURCE,
    RECYCLED_ARTWORK_CELL_RESOURCE,
    RECYCLED_BADGED_TEXT_CELL_RESOURCE,
    RECYCLED_FOLDER_CELL_RESOURCE,
    RECYCLED_MERGED_CELL_RESOURCE,
    RECYCLED_TEXT_CELL_RESOURCE,
    SEARCH_ROUTE_RESOURCE,
    RANDOM_PLAY_RESOURCE,
    SHORTCUTS_RESOURCE,
];

pub(crate) fn builder(resource: &str) -> gtk::Builder {
    let builder = gtk::Builder::new();
    builder
        .add_from_resource(resource)
        .unwrap_or_else(|error| panic!("failed to load interface resource {resource}: {error}"));
    builder
}

pub(crate) fn object<T>(builder: &gtk::Builder, resource: &str, id: &str) -> T
where
    T: IsA<glib::Object>,
{
    builder
        .object(id)
        .unwrap_or_else(|| panic!("interface resource {resource} is missing object {id}"))
}
