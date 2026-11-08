use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;

use crate::widgets::layout::configure_fill_width_clip;

const ROUTE_SCROLL_OWNER_CLASS: &str = "route-scroll-owner";
/// Under this width a detail page's cover takes its own row.
pub const DETAIL_SHOWCASE_STACKED_WIDTH: i32 = 560;
pub const ROUTE_TOP_MARGIN: i32 = 8;
pub const PRIMARY_ROUTE_MARGIN_START: i32 = 10;
pub const PRIMARY_ROUTE_MARGIN_END: i32 = 10;
pub const PRIMARY_ROUTE_HORIZONTAL_INSET: i32 =
    PRIMARY_ROUTE_MARGIN_START + PRIMARY_ROUTE_MARGIN_END;

crate::composite_box!(
    pub RoutePlaceholderView,
    route_placeholder_view_imp,
    "RufinRoutePlaceholderView",
    "/io/github/screwys/Rufin/ui/routes/placeholder.ui",
    {
        heading: gtk::Label,
        body: gtk::Label,
    }
);

pub fn detail_route_inner_width_for_viewport(viewport_width: i32, horizontal_inset: i32) -> i32 {
    viewport_width
        .saturating_sub(horizontal_inset)
        .saturating_sub(PRIMARY_ROUTE_MARGIN_END)
        .max(1)
}

/// A detail page's cover side at `width`: centred in its own row when narrow, beside the
/// text from [`DETAIL_SHOWCASE_STACKED_WIDTH`], growing to 224 px.
pub fn detail_showcase_cover_size(width: i32) -> i32 {
    if width < DETAIL_SHOWCASE_STACKED_WIDTH {
        (width - 32).clamp(72, 196)
    } else {
        164 + (width - DETAIL_SHOWCASE_STACKED_WIDTH).min(200) * 60 / 200
    }
}

pub fn home_album_content_width_for(width: i32) -> i32 {
    (width.max(1) - PRIMARY_ROUTE_HORIZONTAL_INSET).max(1)
}

pub fn mark_route_scroll_owner(scroller: &gtk::ScrolledWindow) {
    scroller.add_css_class(ROUTE_SCROLL_OWNER_CLASS);
}

pub fn primary_route_scroll_adjustment(root: &gtk::Widget) -> Option<gtk::Adjustment> {
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

pub fn route_boundary(view: gtk::Widget) -> gtk::Widget {
    let scroller = gtk::ScrolledWindow::new();
    // Oversized route headers must scroll instead of raising the window's minimum height.
    configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(false);
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&view));
    scroller.upcast::<gtk::Widget>()
}

pub fn route_scroller_widget(scroller: gtk::ScrolledWindow) -> gtk::Widget {
    let (_, vertical_policy) = scroller.policy();
    if vertical_policy != gtk::PolicyType::Never {
        mark_route_scroll_owner(&scroller);
        scroller.set_overlay_scrolling(true);
    }
    scroller.upcast()
}

pub fn detail_route_scroller(content: gtk::Widget) -> gtk::ScrolledWindow {
    let scroller = gtk::ScrolledWindow::new();
    configure_library_route_scroller(&scroller);
    scroller.set_child(Some(&content));
    scroller
}

pub fn detail_route_wrapper(spacing: i32) -> gtk::Box {
    let wrapper = gtk::Box::new(gtk::Orientation::Vertical, spacing);
    wrapper.add_css_class("route-content");
    wrapper.set_hexpand(true);
    wrapper.set_halign(gtk::Align::Fill);
    wrapper.set_width_request(1);
    wrapper.set_vexpand(true);
    wrapper
}

pub fn placeholder_view(title: &str, body: &str) -> gtk::Widget {
    let view = RoutePlaceholderView::new();
    view.imp().heading.set_label(&localization::tr(title));
    view.imp().body.set_label(&localization::tr(body));
    view.upcast()
}

pub fn route_empty_view(body: &str) -> gtk::Widget {
    let view = RoutePlaceholderView::new();
    view.imp().heading.set_visible(false);
    view.imp().body.set_label(&localization::tr(body));
    view.upcast()
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
}

pub fn configure_library_route_scroller(scroller: &gtk::ScrolledWindow) {
    scroller.add_css_class("library-route-scroller");
    scroller.add_css_class("route-scroll-owner");
    configure_fill_width_clip(scroller, gtk::PolicyType::Automatic);
    scroller.set_propagate_natural_height(false);
    scroller.set_overlay_scrolling(true);
    scroller.set_hexpand(true);
    scroller.set_vexpand(true);
}
