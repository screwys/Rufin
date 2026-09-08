use gtk::glib;
use gtk::prelude::*;

#[macro_export]
macro_rules! composite_box {
    ($vis:vis $name:ident, $imp:ident, $gtype:literal, $resource:literal, {
        $($field:ident: $type:ty),+ $(,)?
    }) => {
        $vis mod $imp {
            use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};

            #[derive(CompositeTemplate, Default)]
            #[template(resource = $resource)]
            $vis struct $name {
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

#[macro_export]
macro_rules! objects {
    ($builder:expr, $resource:expr, { $($name:ident: $type:ty),+ $(,)? }) => {
        $(
            let $name: $type =
                $crate::ui_resource::object(&$builder, $resource, stringify!($name));
        )+
    };
}

pub fn builder(resource: &str) -> gtk::Builder {
    let builder = gtk::Builder::new();
    builder
        .add_from_resource(resource)
        .unwrap_or_else(|error| panic!("failed to load interface resource {resource}: {error}"));
    builder
}

pub fn object<T>(builder: &gtk::Builder, resource: &str, id: &str) -> T
where
    T: IsA<glib::Object>,
{
    builder
        .object(id)
        .unwrap_or_else(|| panic!("interface resource {resource} is missing object {id}"))
}

pub const RECYCLED_ARTWORK_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_artwork_cell.ui";
pub const RECYCLED_BADGED_TEXT_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_badged_text_cell.ui";
pub const RECYCLED_FOLDER_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_folder_cell.ui";
pub const RECYCLED_MERGED_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_merged_cell.ui";
pub const RECYCLED_TEXT_CELL_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/recycled_text_cell.ui";

pub const INTERFACE_RESOURCE_PATHS: &[&str] = &[
    PLAYLIST_PICKER_RESOURCE,
    PLAYLIST_PICKER_CONTEXT_RESOURCE,
    PLAYLIST_PICKER_ROW_RESOURCE,
    MANAGE_SERVER_RESOURCE,
    SMART_PLAYLIST_DIALOG_RESOURCE,
    PLAYLIST_NAME_DIALOG_RESOURCE,
    METADATA_DIALOG_RESOURCE,
    RECYCLED_ARTWORK_CELL_RESOURCE,
    RECYCLED_BADGED_TEXT_CELL_RESOURCE,
    RECYCLED_FOLDER_CELL_RESOURCE,
    RECYCLED_MERGED_CELL_RESOURCE,
    RECYCLED_TEXT_CELL_RESOURCE,
];

pub const METADATA_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/metadata.ui";

pub const PLAYLIST_NAME_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/playlist_name.ui";

pub const SMART_PLAYLIST_DIALOG_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/dialogs/smart_playlist.ui";

pub const MANAGE_SERVER_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/preferences/source/manage_server.ui";
pub const PLAYLIST_PICKER_RESOURCE: &str = "/io/github/screwys/Rufin/ui/routes/playlist_picker.ui";
pub const PLAYLIST_PICKER_CONTEXT_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/playlist_picker_context.ui";
pub const PLAYLIST_PICKER_ROW_RESOURCE: &str =
    "/io/github/screwys/Rufin/ui/routes/playlist_picker_row.ui";

pub fn verify_icons() -> Result<(), String> {
    crate::register_resources()?;
    for relative_path in [
        "scalable/apps/io.github.screwys.Rufin.svg",
        "64x64/apps/io.github.screwys.Rufin.source.webdav.png",
        "64x64/apps/io.github.screwys.Rufin.source.smb.png",
        "scalable/apps/io.github.screwys.Rufin.source.plex.svg",
        "scalable/apps/io.github.screwys.Rufin.source.emby.svg",
        "scalable/actions/rufin-go-last-symbolic.svg",
        "scalable/actions/rufin-audio-only-symbolic.svg",
        "scalable/actions/rufin-library-music-symbolic.svg",
        "scalable/actions/rufin-mail-forward-symbolic.svg",
        "scalable/actions/rufin-media-playback-start-symbolic.svg",
        "scalable/actions/rufin-media-skip-backward-symbolic.svg",
        "scalable/actions/rufin-media-skip-forward-symbolic.svg",
        "scalable/actions/rufin-moods-symbolic.svg",
        "scalable/actions/rufin-music-queue-symbolic.svg",
        "scalable/actions/rufin-open-menu-symbolic.svg",
        "scalable/actions/rufin-player-more-symbolic.svg",
        "scalable/actions/rufin-preferences-desktop-appearance-symbolic.svg",
        "scalable/actions/rufin-search-symbolic.svg",
        "scalable/actions/rufin-tag-outline-symbolic.svg",
        "scalable/actions/rufin-x-office-calendar-symbolic.svg",
        "symbolic/apps/io.github.screwys.Rufin-symbolic.svg",
    ] {
        let path = format!("/io/github/screwys/Rufin/icons/hicolor/{relative_path}");
        gtk::gio::resources_lookup_data(&path, gtk::gio::ResourceLookupFlags::NONE)
            .map_err(|error| format!("missing compiled Rufin resource {path}: {error}"))?;
    }
    Ok(())
}

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

    #[test]
    fn representative_rufin_icons_are_compiled_resources() {
        super::verify_icons().expect("compiled icon resources");
    }
}
