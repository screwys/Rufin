use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;

use crate::layout::configure_fill_width_clip;
use crate::shell::Shell;
use crate::shell::layout::route_content_width;

use super::collections::configure_library_route_scroller;
use super::library_fields::COLLECTION_GRID_CARD_MARGIN;

const ROUTE_SCROLL_OWNER_CLASS: &str = "route-scroll-owner";
const DETAIL_SHOWCASE_MIN_COVER_SIZE: i32 = 150;
pub(crate) const DETAIL_SHOWCASE_METADATA_MIN_WIDTH: i32 = 430;
const DETAIL_SHOWCASE_COMPACT_WIDTH: i32 = 760;
const DETAIL_SHOWCASE_MAX_COVER_SIZE: i32 = 224;
pub(crate) const ROUTE_TOP_MARGIN: i32 = 10;
pub(crate) const PRIMARY_ROUTE_MARGIN_START: i32 = 10;
pub(crate) const PRIMARY_ROUTE_MARGIN_END: i32 = 10;
pub(crate) const PRIMARY_ROUTE_HORIZONTAL_INSET: i32 =
    PRIMARY_ROUTE_MARGIN_START + PRIMARY_ROUTE_MARGIN_END;

crate::ui_resource::composite_box!(
    pub(crate) RoutePlaceholderView,
    route_placeholder_view_imp,
    "RufinRoutePlaceholderView",
    "/io/github/screwys/Rufin/ui/routes/placeholder.ui",
    {
        heading: gtk::Label,
        body: gtk::Label,
    }
);

pub(crate) fn home_album_content_width(shell: &Shell) -> i32 {
    home_album_content_width_for(route_content_width(shell))
}

pub(crate) fn detail_route_inner_width(shell: &Shell, horizontal_inset: i32) -> i32 {
    detail_route_inner_width_for_viewport(route_content_width(shell), horizontal_inset)
}

pub(crate) fn detail_route_inner_width_for_viewport(
    viewport_width: i32,
    horizontal_inset: i32,
) -> i32 {
    viewport_width
        .saturating_sub(horizontal_inset)
        .saturating_sub(PRIMARY_ROUTE_MARGIN_END)
        .max(1)
}

pub(crate) fn detail_showcase_cover_size(width: i32) -> i32 {
    if width < DETAIL_SHOWCASE_METADATA_MIN_WIDTH {
        width.clamp(72, DETAIL_SHOWCASE_MIN_COVER_SIZE)
    } else if width < DETAIL_SHOWCASE_COMPACT_WIDTH {
        DETAIL_SHOWCASE_MIN_COVER_SIZE
            + ((width - DETAIL_SHOWCASE_METADATA_MIN_WIDTH)
                * (DETAIL_SHOWCASE_MAX_COVER_SIZE - DETAIL_SHOWCASE_MIN_COVER_SIZE)
                / (DETAIL_SHOWCASE_COMPACT_WIDTH - DETAIL_SHOWCASE_METADATA_MIN_WIDTH))
    } else {
        DETAIL_SHOWCASE_MAX_COVER_SIZE
    }
}

pub(crate) fn detail_showcase_cover_only(width: i32) -> bool {
    width < DETAIL_SHOWCASE_METADATA_MIN_WIDTH
}

pub(crate) fn home_album_content_width_for(width: i32) -> i32 {
    (width.max(1) - COLLECTION_GRID_CARD_MARGIN - PRIMARY_ROUTE_MARGIN_END).max(1)
}

pub(crate) fn mark_route_scroll_owner(scroller: &gtk::ScrolledWindow) {
    scroller.add_css_class(ROUTE_SCROLL_OWNER_CLASS);
}

pub(crate) fn primary_route_scroll_adjustment(root: &gtk::Widget) -> Option<gtk::Adjustment> {
    let mut pending = vec![root.clone()];
    while let Some(widget) = pending.pop() {
        if let Some(scroller) = widget.downcast_ref::<gtk::ScrolledWindow>()
            && scroller.has_css_class(ROUTE_SCROLL_OWNER_CLASS)
        {
            return Some(scroller.vadjustment());
        }

        let mut children = Vec::new();
        let mut child = widget.first_child();
        while let Some(current) = child {
            child = current.next_sibling();
            children.push(current);
        }
        pending.extend(children.into_iter().rev());
    }
    None
}

pub(crate) fn route_boundary(view: gtk::Widget) -> gtk::Widget {
    let scroller = gtk::ScrolledWindow::new();
    configure_fill_width_clip(&scroller, gtk::PolicyType::Never);
    scroller.set_propagate_natural_height(false);
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&view));
    scroller.upcast::<gtk::Widget>()
}

pub(crate) fn route_scroller_widget(scroller: gtk::ScrolledWindow) -> gtk::Widget {
    let (_, vertical_policy) = scroller.policy();
    if vertical_policy != gtk::PolicyType::Never {
        mark_route_scroll_owner(&scroller);
        scroller.set_overlay_scrolling(true);
    }
    scroller.upcast()
}

pub(crate) fn detail_route_scroller(content: gtk::Widget) -> gtk::ScrolledWindow {
    let scroller = gtk::ScrolledWindow::new();
    configure_library_route_scroller(&scroller);
    scroller.set_child(Some(&content));
    scroller
}

pub(crate) fn detail_route_wrapper(spacing: i32) -> gtk::Box {
    let wrapper = gtk::Box::new(gtk::Orientation::Vertical, spacing);
    wrapper.add_css_class("route-content");
    wrapper.set_hexpand(true);
    wrapper.set_halign(gtk::Align::Fill);
    wrapper.set_width_request(1);
    wrapper.set_vexpand(true);
    wrapper
}

impl Shell {
    pub(crate) fn placeholder_view(&self, title: &str, body: &str) -> gtk::Widget {
        let view = RoutePlaceholderView::new();
        view.imp().heading.set_label(&localization::tr(title));
        view.imp().body.set_label(&localization::tr(body));
        view.upcast()
    }

    pub(crate) fn route_empty_view(&self, body: &str) -> gtk::Widget {
        let view = RoutePlaceholderView::new();
        view.imp().heading.set_visible(false);
        view.imp().body.set_label(&localization::tr(body));
        view.upcast()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_inner_width_comes_from_the_route_viewport() {
        assert_eq!(
            detail_route_inner_width_for_viewport(900, PRIMARY_ROUTE_MARGIN_START),
            880
        );
        assert_eq!(
            detail_route_inner_width_for_viewport(8, PRIMARY_ROUTE_MARGIN_START),
            1
        );
    }

    #[test]
    #[ignore = "requires a GTK display"]
    fn route_placeholder_template_builds() {
        gtk::init().expect("GTK display");
        crate::application::verify_interface_resources().expect("compiled interface resources");
        let view = RoutePlaceholderView::new();
        assert!(view.imp().heading.is_visible());
        assert!(view.imp().body.wraps());
    }
}
