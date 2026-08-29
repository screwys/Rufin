use adw::prelude::*;
use app_identity::DISPLAY_NAME;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use std::{cell::Cell, rc::Rc};

use crate::layout::configure_fill_width_clip;
use crate::routes::route_layout::ROUTE_TOP_MARGIN;
use localization::tr;

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
use super::layout::{COMPACT_RAIL_WIDTH, WINDOW_CHROME_MARGIN_END};

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const WINDOW_START_CONTROLS_MARGIN_TOP: i32 = 6;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const WINDOW_END_CONTROLS_MARGIN_TOP: i32 = 10;
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const WINDOW_CHROME_MARGIN_START: i32 = 8;
const WINDOW_DRAG_HANDLE_HEIGHT: i32 = ROUTE_TOP_MARGIN;
const WINDOW_DRAG_HANDLE_MARGIN_START: i32 = 56;
pub(super) const RIGHT_RESIZE_HANDLE_WIDTH: i32 = 4;
pub(crate) const ROUTE_VIEWPORT_CLASS: &str = "route-viewport";

pub(crate) struct WindowChrome {
    pub(crate) application: adw::Application,
    pub(crate) window: gtk::ApplicationWindow,
    pub(crate) window_controls: WindowControlLayout,
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
    pub(super) login_host: gtk::Box,
    pub(super) startup_loading_host: gtk::Box,
}

pub(crate) struct WindowControlLayout {
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    platform_bar: bool,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    start: gtk::WindowControls,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    start_alignment: gtk::CenterBox,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    start_host: gtk::Box,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    end: gtk::WindowControls,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    end_host: gtk::Box,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    start_width: gtk::SizeGroup,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    start_height: gtk::SizeGroup,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    end_width: gtk::SizeGroup,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    decoration_settings: Option<gtk::Settings>,
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    full_controls_allowed: Rc<Cell<(bool, bool)>>,
}

impl WindowControlLayout {
    pub(crate) fn new(platform_bar_preview: bool) -> Self {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let start = gtk::WindowControls::new(gtk::PackType::Start);
            if platform_bar_preview {
                start.set_decoration_layout(Some(":"));
            }
            let start_host = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            start_host.add_css_class("window-controls");
            start_host.set_halign(gtk::Align::Start);
            start_host.set_valign(gtk::Align::Start);
            start_host.set_margin_top(WINDOW_START_CONTROLS_MARGIN_TOP);
            start_host.set_margin_start(WINDOW_CHROME_MARGIN_START);
            let start_alignment = gtk::CenterBox::new();
            start_alignment.set_center_widget(Some(&start));
            start_host.append(&start_alignment);

            let end = gtk::WindowControls::new(gtk::PackType::End);
            if platform_bar_preview {
                end.set_decoration_layout(Some(":"));
            }
            let end_host = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            end_host.add_css_class("window-controls");
            end_host.set_halign(gtk::Align::End);
            end_host.set_valign(gtk::Align::Start);
            end_host.set_margin_top(WINDOW_END_CONTROLS_MARGIN_TOP);
            end_host.set_margin_end(WINDOW_CHROME_MARGIN_END);
            end_host.append(&end);

            let start_width = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
            start_width.add_widget(&start);
            let start_height = gtk::SizeGroup::new(gtk::SizeGroupMode::Vertical);
            start_height.add_widget(&start);
            let end_width = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
            end_width.add_widget(&end);

            let decoration_settings = (!platform_bar_preview)
                .then(gtk::Settings::default)
                .flatten();
            let full_controls_allowed = Rc::new(Cell::new((true, true)));
            if let Some(settings) = decoration_settings.as_ref() {
                apply_window_control_layout(&start, &end, settings, true, true);

                let weak_start = start.downgrade();
                let weak_end = end.downgrade();
                let full_controls_allowed = Rc::clone(&full_controls_allowed);
                settings.connect_gtk_decoration_layout_notify(move |settings| {
                    let Some(start) = weak_start.upgrade() else {
                        return;
                    };
                    let Some(end) = weak_end.upgrade() else {
                        return;
                    };
                    let (start_allowed, end_allowed) = full_controls_allowed.get();
                    apply_window_control_layout(&start, &end, settings, start_allowed, end_allowed);
                });
            }

            Self {
                platform_bar: platform_bar_preview,
                start,
                start_alignment,
                start_host,
                end,
                end_host,
                start_width,
                start_height,
                end_width,
                decoration_settings,
                full_controls_allowed,
            }
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            let _ = platform_bar_preview;
            Self {}
        }
    }

    pub(crate) fn wrap_content(&self, content: &impl IsA<gtk::Widget>) -> gtk::Widget {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let root = gtk::Overlay::new();
            root.set_hexpand(true);
            root.set_vexpand(true);
            root.set_child(Some(content));
            root.add_overlay(&self.start_host);
            root.set_measure_overlay(&self.start_host, false);
            root.add_overlay(&self.end_host);
            root.set_measure_overlay(&self.end_host, false);
            root.upcast()
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        content.as_ref().clone()
    }

    pub(crate) fn bind_window(&self, window: &gtk::ApplicationWindow) {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        for controls in [&self.start_host, &self.end_host] {
            window
                .bind_property("fullscreened", controls, "visible")
                .sync_create()
                .invert_boolean()
                .build();
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let _ = window;
    }

    pub(crate) fn start_width_reservation(&self) -> gtk::Box {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            control_reservation(&self.start, &self.start_width, 0)
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        hidden_control_reservation()
    }

    pub(crate) fn set_compact_start_alignment(&self, compact: bool) {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            self.start_host.set_margin_start(if compact {
                0
            } else {
                WINDOW_CHROME_MARGIN_START
            });
            self.start_alignment
                .set_width_request(if compact { COMPACT_RAIL_WIDTH } else { -1 });
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let _ = compact;
    }

    pub(crate) fn set_full_controls_allowed(&self, start: bool, end: bool) {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            if self.platform_bar {
                return;
            }
            if self.full_controls_allowed.replace((start, end)) == (start, end) {
                return;
            }
            if let Some(settings) = self.decoration_settings.as_ref() {
                apply_window_control_layout(&self.start, &self.end, settings, start, end);
            }
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let _ = (start, end);
    }

    pub(crate) fn compact_start_reservation(&self) -> gtk::Box {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            control_reservation(
                &self.start,
                &self.start_height,
                WINDOW_START_CONTROLS_MARGIN_TOP,
            )
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        hidden_control_reservation()
    }

    pub(crate) fn end_width_reservation(&self) -> gtk::Box {
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            control_reservation(&self.end, &self.end_width, 0)
        }

        #[cfg(any(target_os = "windows", target_os = "macos"))]
        hidden_control_reservation()
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn apply_window_control_layout(
    start: &gtk::WindowControls,
    end: &gtk::WindowControls,
    settings: &gtk::Settings,
    full_start_controls: bool,
    full_end_controls: bool,
) {
    let layout = settings
        .gtk_decoration_layout()
        .map(|layout| filtered_decoration_layout(&layout, full_start_controls, full_end_controls));
    start.set_decoration_layout(layout.as_deref());
    end.set_decoration_layout(layout.as_deref());
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn filtered_decoration_layout(
    layout: &str,
    full_start_controls: bool,
    full_end_controls: bool,
) -> String {
    let filter_side = |side: &str, full_controls: bool| {
        side.split(',')
            .map(str::trim)
            .filter(|token| !token.is_empty())
            .filter(|token| *token != "icon")
            .filter(|token| full_controls || *token == "close")
            .collect::<Vec<_>>()
            .join(",")
    };

    match layout.split_once(':') {
        Some((start, end)) => format!(
            "{}:{}",
            filter_side(start, full_start_controls),
            filter_side(end, full_end_controls)
        ),
        None => filter_side(layout, full_start_controls),
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn control_reservation(
    controls: &gtk::WindowControls,
    size_group: &gtk::SizeGroup,
    margin_top: i32,
) -> gtk::Box {
    let reservation = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    reservation.set_margin_top(margin_top);
    size_group.add_widget(&reservation);
    controls
        .bind_property("empty", &reservation, "visible")
        .sync_create()
        .invert_boolean()
        .build();
    reservation
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn hidden_control_reservation() -> gtk::Box {
    let reservation = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    reservation.set_visible(false);
    reservation
}

pub(super) struct MainAreaParts {
    pub(super) root: adw::ToolbarView,
    pub(super) route_host: gtk::Stack,
}

pub(super) struct ContentChromeParts {
    pub(super) root: gtk::Overlay,
    pub(super) right_split: gtk::Paned,
    pub(super) right_panel_slot: gtk::ScrolledWindow,
    pub(super) right_resize_handle: gtk::Box,
}

pub(super) fn build_main_area() -> MainAreaParts {
    let root = adw::ToolbarView::new();
    root.add_css_class("main-area");
    root.set_hexpand(true);
    root.set_vexpand(true);

    let route_host = gtk::Stack::new();
    route_host.add_css_class(ROUTE_VIEWPORT_CLASS);
    route_host.set_hhomogeneous(false);
    route_host.set_vhomogeneous(false);
    route_host.set_interpolate_size(false);
    route_host.set_transition_type(gtk::StackTransitionType::None);
    route_host.set_transition_duration(0);
    route_host.set_width_request(1);
    route_host.set_halign(gtk::Align::Fill);
    route_host.set_hexpand(true);
    route_host.set_vexpand(true);
    route_host.set_overflow(gtk::Overflow::Hidden);

    root.set_content(Some(&route_host));

    MainAreaParts { root, route_host }
}

pub(super) fn build_content_chrome(
    main_area: &adw::ToolbarView,
    right_panel: &gtk::Box,
) -> ContentChromeParts {
    let main_well = gtk::Overlay::new();
    main_well.set_overflow(gtk::Overflow::Hidden);
    main_well.set_width_request(1);
    main_well.set_hexpand(true);
    main_well.set_vexpand(true);
    main_area.set_width_request(1);
    main_area.set_halign(gtk::Align::Fill);
    main_area.set_valign(gtk::Align::Fill);
    main_area.set_overflow(gtk::Overflow::Hidden);
    main_well.set_child(Some(main_area));
    let drag_handle = top_window_drag_handle("window-drag-handle");
    main_well.add_overlay(&drag_handle);
    main_well.set_measure_overlay(&drag_handle, false);

    let right_panel_slot = gtk::ScrolledWindow::new();
    configure_fill_width_clip(&right_panel_slot, gtk::PolicyType::Never);
    right_panel_slot.set_propagate_natural_height(false);
    right_panel_slot.set_hexpand(false);
    right_panel_slot.set_vexpand(true);
    right_panel_slot.set_child(Some(right_panel));

    let right_split = gtk::Paned::new(gtk::Orientation::Horizontal);
    configure_right_split(&right_split);
    right_split.set_start_child(Some(&main_well));
    right_split.set_end_child(Some(&right_panel_slot));
    right_split.set_hexpand(true);
    right_split.set_vexpand(true);

    let root = gtk::Overlay::new();
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_child(Some(&right_split));

    let right_resize_handle = gtk::Box::new(gtk::Orientation::Vertical, 0);
    right_resize_handle.add_css_class("right-sidebar-resize-handle");
    right_resize_handle.set_width_request(RIGHT_RESIZE_HANDLE_WIDTH);
    right_resize_handle.set_halign(gtk::Align::Start);
    right_resize_handle.set_valign(gtk::Align::Fill);
    right_resize_handle.set_vexpand(true);
    right_resize_handle.set_focusable(false);
    right_resize_handle.set_cursor_from_name(Some("col-resize"));
    let resize_label = tr("Hold and drag to resize");
    right_resize_handle.update_property(&[gtk::accessible::Property::Label(&resize_label)]);
    right_resize_handle.set_visible(false);
    root.add_overlay(&right_resize_handle);
    root.set_measure_overlay(&right_resize_handle, false);

    ContentChromeParts {
        root,
        right_split,
        right_panel_slot,
        right_resize_handle,
    }
}

fn configure_right_split(right_split: &gtk::Paned) {
    right_split.add_css_class("right-pane-split");
    right_split.set_focusable(false);
    // Rufin owns the four-pixel input target. A non-wide GtkPaned adds a
    // hidden six-pixel pointer gutter on both sides of its separator, which
    // would leave a second resize owner underneath the shell gesture.
    right_split.set_wide_handle(true);
    // The shell sets an exact divider position for the allocation it is about
    // to grant. Preserve that start position when GtkPaned receives its new
    // width instead of applying GtkPaned's own parent-resize distribution to
    // the already-resolved position.
    right_split.set_resize_start_child(false);
    right_split.set_shrink_start_child(true);
    right_split.set_resize_end_child(true);
    right_split.set_shrink_end_child(false);
}

pub(super) fn configure_primary_menu_button(button: &gtk::Button) {
    let label = tr("Menu");
    button.set_tooltip_text(Some(&label));
    button.update_property(&[gtk::accessible::Property::Label(&label)]);
}

pub(crate) fn playback_window_title(title: Option<&str>, artist: Option<&str>) -> String {
    title
        .into_iter()
        .chain(artist)
        .filter(|part| !part.trim().is_empty())
        .chain(std::iter::once(DISPLAY_NAME))
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    use super::filtered_decoration_layout;
    use super::{DISPLAY_NAME, playback_window_title};

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn decoration_layout_removes_window_icon_without_moving_controls() {
        assert_eq!(
            filtered_decoration_layout("icon:minimize,maximize,close", true, true),
            ":minimize,maximize,close"
        );
        assert_eq!(
            filtered_decoration_layout("close,icon:appmenu", true, true),
            "close:appmenu"
        );
        assert_eq!(
            filtered_decoration_layout("appmenu:close", true, true),
            "appmenu:close"
        );
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    #[test]
    fn decoration_layout_keeps_only_close_without_a_full_pane_on_that_side() {
        assert_eq!(
            filtered_decoration_layout("icon:minimize,maximize,close", true, false),
            ":close"
        );
        assert_eq!(
            filtered_decoration_layout("close,icon,minimize,maximize:", false, true),
            "close:"
        );
        assert_eq!(
            filtered_decoration_layout("minimize,maximize,close:", true, false),
            "minimize,maximize,close:"
        );
        assert_eq!(
            filtered_decoration_layout("close:minimize,maximize,close", false, true),
            "close:minimize,maximize,close"
        );
        assert_eq!(
            filtered_decoration_layout("appmenu:close", false, false),
            ":close"
        );
    }

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

pub(crate) fn window_drag_handle_with_child(
    css_class: &str,
    child: &impl IsA<gtk::Widget>,
) -> gtk::WindowHandle {
    let handle = gtk::WindowHandle::new();
    handle.add_css_class(css_class);
    handle.set_halign(gtk::Align::Fill);
    handle.set_valign(gtk::Align::Start);
    handle.set_hexpand(true);
    handle.set_vexpand(false);
    handle.set_child(Some(child));
    handle
}

pub(crate) fn top_window_drag_handle(css_class: &str) -> gtk::WindowHandle {
    window_drag_handle_with_margins(
        css_class,
        WINDOW_DRAG_HANDLE_HEIGHT,
        WINDOW_DRAG_HANDLE_MARGIN_START,
        0,
    )
}

fn window_drag_handle_with_margins(
    css_class: &str,
    height: i32,
    margin_start: i32,
    margin_end: i32,
) -> gtk::WindowHandle {
    let handle =
        window_drag_handle_with_child(css_class, &gtk::Box::new(gtk::Orientation::Horizontal, 0));
    handle.set_margin_start(margin_start);
    handle.set_margin_end(margin_end);
    handle.set_height_request(height);

    if let Some(area) = handle.child() {
        area.set_hexpand(true);
        area.set_height_request(height);
    }
    handle
}
