use adw::prelude::*;

use crate::layout::configure_fill_width_clip;
use crate::localization::localized_label;
use crate::shell::Shell;
use crate::shell::layout::route_content_width;

use super::collections::configure_library_route_scroller;
use super::library_fields::COLLECTION_GRID_CARD_MARGIN;

const ROUTE_SCROLL_OWNER_CLASS: &str = "route-scroll-owner";
pub(crate) const ROUTE_SCROLLBAR_LANE_WIDTH: i32 = 9;
const DETAIL_SHOWCASE_MIN_COVER_SIZE: i32 = 150;
const DETAIL_SHOWCASE_TEXT_MIN_WIDTH: i32 = 420;
const DETAIL_SHOWCASE_COMPACT_WIDTH: i32 = 760;
const DETAIL_SHOWCASE_MAX_COVER_SIZE: i32 = 224;
pub(crate) const ROUTE_TOP_MARGIN: i32 = 10;
pub(crate) const PRIMARY_ROUTE_MARGIN_START: i32 = ROUTE_TOP_MARGIN;
pub(crate) const PRIMARY_ROUTE_MARGIN_END: i32 = ROUTE_SCROLLBAR_LANE_WIDTH;
pub(crate) const PRIMARY_ROUTE_HORIZONTAL_INSET: i32 =
    PRIMARY_ROUTE_MARGIN_START + PRIMARY_ROUTE_MARGIN_END;

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
    if width < DETAIL_SHOWCASE_TEXT_MIN_WIDTH {
        width.clamp(72, DETAIL_SHOWCASE_MIN_COVER_SIZE)
    } else if width < DETAIL_SHOWCASE_COMPACT_WIDTH {
        DETAIL_SHOWCASE_MIN_COVER_SIZE
            + ((width - DETAIL_SHOWCASE_TEXT_MIN_WIDTH)
                * (DETAIL_SHOWCASE_MAX_COVER_SIZE - DETAIL_SHOWCASE_MIN_COVER_SIZE)
                / (DETAIL_SHOWCASE_COMPACT_WIDTH - DETAIL_SHOWCASE_TEXT_MIN_WIDTH))
    } else {
        DETAIL_SHOWCASE_MAX_COVER_SIZE
    }
}

pub(crate) fn detail_showcase_cover_only(width: i32) -> bool {
    width < DETAIL_SHOWCASE_TEXT_MIN_WIDTH
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
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 12);
        wrapper.add_css_class("empty-state");
        wrapper.set_vexpand(true);
        wrapper.set_hexpand(true);
        wrapper.set_valign(gtk::Align::Center);
        wrapper.set_halign(gtk::Align::Center);

        let heading = localized_label(title);
        heading.add_css_class("section-heading");
        let label = localized_label(body);
        label.add_css_class("muted");
        label.set_wrap(true);
        label.set_justify(gtk::Justification::Center);
        wrapper.append(&heading);
        wrapper.append(&label);
        wrapper.upcast()
    }

    pub(crate) fn route_empty_view(&self, body: &str) -> gtk::Widget {
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 12);
        wrapper.add_css_class("empty-state");
        wrapper.set_vexpand(true);
        wrapper.set_hexpand(true);
        wrapper.set_valign(gtk::Align::Center);
        wrapper.set_halign(gtk::Align::Center);

        let label = localized_label(body);
        label.add_css_class("muted");
        label.set_wrap(true);
        label.set_justify(gtk::Justification::Center);
        wrapper.append(&label);
        wrapper.upcast()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_inner_width_comes_from_the_route_viewport() {
        assert_eq!(
            detail_route_inner_width_for_viewport(900, PRIMARY_ROUTE_MARGIN_START),
            881
        );
        assert_eq!(
            detail_route_inner_width_for_viewport(8, PRIMARY_ROUTE_MARGIN_START),
            1
        );
    }
}
