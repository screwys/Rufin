use adw::prelude::*;
#[cfg(test)]
use app_identity::DISPLAY_NAME;
#[cfg(test)]
use ui_player::state::playback_window_title;

use localization::tr;
use ui_shared::layout::configure_fill_width_clip;

pub(super) const RIGHT_RESIZE_HANDLE_WIDTH: i32 = 8;
pub(crate) struct WindowChrome {
    pub(crate) application: adw::Application,
    pub(crate) window: gtk::ApplicationWindow,
    pub(crate) topbar: super::topbar::Topbar,
    pub(crate) toast_overlay: adw::ToastOverlay,
    pub(super) control_feedback_label: gtk::Label,
    pub(crate) source_refresh_feedback: gtk::Box,
    pub(crate) source_refresh_feedback_label: gtk::Label,
    pub(crate) source_refresh_feedback_progress: gtk::ProgressBar,
    pub(crate) operation_feedback: gtk::Box,
    pub(crate) operation_feedback_artwork: gtk::Box,
    pub(crate) operation_feedback_title: gtk::Label,
    pub(crate) operation_feedback_subtitle: gtk::Label,
    pub(crate) operation_feedback_action: gtk::Button,
    pub(super) root_stack: gtk::Stack,
    pub(crate) app_root_overlay: gtk::Overlay,
    pub(crate) app_content_stack: gtk::Stack,
    pub(super) startup_loading_host: gtk::Box,
    pub(super) startup_loading_status: gtk::Label,
}

pub(super) struct ContentChromeParts {
    pub(super) root: gtk::Overlay,
    pub(super) route_host: gtk::Stack,
    pub(super) route_loading: gtk::Box,
    pub(super) right_split: gtk::Paned,
    pub(super) right_panel_slot: gtk::ScrolledWindow,
    pub(super) right_resize_handle: gtk::Box,
}

pub(super) fn build_content_chrome(right_panel: &gtk::Box) -> ContentChromeParts {
    let resource = crate::ui_resource::CONTENT_CHROME_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        root: gtk::Overlay,
        main_well: gtk::Overlay,
        route_host: gtk::Stack,
        route_loading: gtk::Box,
        right_split: gtk::Paned,
        right_panel_slot: gtk::ScrolledWindow,
        right_resize_handle: gtk::Box,
    });
    main_well.set_measure_overlay(&route_loading, false);

    configure_fill_width_clip(&right_panel_slot, gtk::PolicyType::Never);
    right_panel_slot.set_child(Some(right_panel));

    right_resize_handle.set_width_request(RIGHT_RESIZE_HANDLE_WIDTH);
    right_resize_handle.set_cursor_from_name(Some("col-resize"));
    let resize_label = tr("Hold and drag to resize");
    right_resize_handle.update_property(&[gtk::accessible::Property::Label(&resize_label)]);
    root.set_measure_overlay(&right_resize_handle, false);

    ContentChromeParts {
        root,
        route_host,
        route_loading,
        right_split,
        right_panel_slot,
        right_resize_handle,
    }
}

#[cfg(test)]
mod tests {
    use super::{DISPLAY_NAME, playback_window_title};
    #[test]
    fn playback_title_contains_track_artist_and_app() {
        assert_eq!(
            playback_window_title(Some("North Star"), Some("The Satellites")),
            format!("North Star · The Satellites · {DISPLAY_NAME}")
        );
    }

    #[test]
    fn playback_title_omits_blank_metadata() {
        assert_eq!(
            playback_window_title(Some("North Star"), Some("  ")),
            format!("North Star · {DISPLAY_NAME}")
        );
        assert_eq!(
            playback_window_title(Some(""), Some("The Satellites")),
            format!("The Satellites · {DISPLAY_NAME}")
        );
    }

    #[test]
    fn playback_title_falls_back_to_app_name() {
        assert_eq!(playback_window_title(None, None), DISPLAY_NAME);
    }
}
