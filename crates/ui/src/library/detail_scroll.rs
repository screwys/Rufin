//! A page that scrolls its header away: a strip fades in as the header leaves and stays at the
//! top. A detail page scrolls as one with its list: the header and the toolbar leave with the
//! rows, and the list stops under the strip.
//!
//! Such a page is as tall as the viewport and what leaves, so the list keeps the size it has
//! under the strip and only moves. The page's position scrolls the page up to there, then the
//! list.

use std::cell::{Cell, RefCell};

use gtk::{glib, prelude::*, subclass::prelude::*};

use crate::library::route_layout::configure_library_route_scroller;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct DetailScroll {
        pub(super) page: RefCell<Option<gtk::Widget>>,
        /// The header in `page`, which the strip stands in for as it leaves.
        pub(super) header: RefCell<Option<gtk::Widget>>,
        pub(super) strip: RefCell<Option<gtk::Widget>>,
        /// The list's own scroller, which this page's position drives.
        pub(super) list: RefCell<Option<gtk::ScrolledWindow>>,
        /// The list with its column header; its top is where the page stops leaving.
        pub(super) frame: RefCell<Option<gtk::Widget>>,
        pub(super) hadjustment: RefCell<Option<gtk::Adjustment>>,
        pub(super) vadjustment: RefCell<Option<gtk::Adjustment>>,
        pub(super) scrolled: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) list_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        /// What is above the list's frame, less the strip: what leaves before the strip shows.
        pub(super) above: Cell<i32>,
        /// How far the page leaves before the list scrolls: `above`, less where the rows end
        /// sooner; without a shown list, to the page's end.
        pub(super) lead: Cell<i32>,
        /// Whether the list shows, so the strip can stay above it.
        pub(super) pinned: Cell<bool>,
        pub(super) viewport: Cell<i32>,
        /// The strip's height with its margins: what of the viewport it covers.
        pub(super) covered: Cell<i32>,
        /// This page is moving the list, or laying it out: the list's position follows.
        pub(super) syncing: Cell<bool>,
        /// The window whose focus this page follows while it shows.
        pub(super) focus: RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DetailScroll {
        const NAME: &'static str = "RufinDetailScroll";
        type Type = super::DetailScroll;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable,);
    }

    impl ObjectImpl for DetailScroll {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: std::sync::OnceLock<Vec<glib::ParamSpec>> =
                std::sync::OnceLock::new();
            PROPERTIES.get_or_init(|| {
                [
                    "hadjustment",
                    "vadjustment",
                    "hscroll-policy",
                    "vscroll-policy",
                ]
                .into_iter()
                .map(glib::ParamSpecOverride::for_interface::<gtk::Scrollable>)
                .collect()
            })
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "hadjustment" => {
                    self.hadjustment
                        .replace(value.get().expect("an adjustment"));
                }
                "vadjustment" => self.set_vadjustment(value.get().expect("an adjustment")),
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "hadjustment" => self.hadjustment.borrow().to_value(),
                "vadjustment" => self.vadjustment.borrow().to_value(),
                _ => gtk::ScrollablePolicy::Minimum.to_value(),
            }
        }

        fn dispose(&self) {
            self.set_vadjustment(None);
            if let Some(list) = self.list.take() {
                let rows = list.vadjustment();
                for handler in self.list_handlers.take() {
                    rows.disconnect(handler);
                }
            }
            self.frame.take();
            self.header.take();
            if let Some(page) = self.page.take() {
                page.unparent();
            }
            if let Some(strip) = self.strip.take() {
                strip.unparent();
            }
        }
    }

    impl WidgetImpl for DetailScroll {
        fn realize(&self) {
            self.parent_realize();
            let Some(window) = self.obj().root().and_downcast::<gtk::Window>() else {
                return;
            };
            let weak = self.obj().downgrade();
            let handler = window.connect_focus_widget_notify(move |window| {
                if let Some(obj) = weak.upgrade() {
                    obj.imp()
                        .show_focus(gtk::prelude::GtkWindowExt::focus(window));
                }
            });
            self.focus.replace(Some((window.downgrade(), handler)));
        }

        fn unrealize(&self) {
            if let Some((window, handler)) = self.focus.take()
                && let Some(window) = window.upgrade()
            {
                window.disconnect(handler);
            }
            self.parent_unrealize();
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let page = self.page.borrow().clone();
            match (orientation, page) {
                (gtk::Orientation::Horizontal, Some(page)) => page.measure(orientation, for_size),
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let (Some(page), Some(strip)) =
                (self.page.borrow().clone(), self.strip.borrow().clone())
            else {
                return;
            };
            self.viewport.set(height);
            let frame = self.frame.borrow().clone();
            let list = self.list.borrow().clone();
            let pinned = frame.as_ref().is_some_and(|frame| shows(frame, &page));
            self.pinned.set(pinned);
            let content = page.measure(gtk::Orientation::Vertical, width).0;
            let strip_height = strip.measure(gtk::Orientation::Vertical, width).0;
            let place = || {
                let tall = if pinned {
                    height + self.above.get()
                } else {
                    content.max(height)
                };
                let up = gtk::gsk::Transform::new()
                    .translate(&gtk::graphene::Point::new(0.0, -(self.offset() as f32)));
                // A list laid out again may move itself to keep a row in place.
                self.syncing.set(true);
                page.allocate(width, tall, -1, Some(up));
                self.syncing.set(false);
            };
            place();
            // Where a part of the page starts and ends in the page unscrolled.
            let obj = self.obj();
            let span = |widget: &gtk::Widget| {
                let offset = self.offset();
                widget.compute_bounds(&*obj).map_or((0, 0), |bounds| {
                    let top = bounds.y().round() as i32 + offset;
                    (top, top + bounds.height().round() as i32)
                })
            };
            let top = |widget: &gtk::Widget| span(widget).0;
            let header_end = self
                .header
                .borrow()
                .as_ref()
                .filter(|header| shows(header, &page))
                .map_or(0, |header| span(header).1);
            let (above, lead) = match (frame, list) {
                (Some(frame), Some(list)) if pinned => {
                    let above = (top(&frame) - strip_height).max(0);
                    let rows = list.vadjustment();
                    let room = rows.page_size() as i32;
                    // Rows that end above the viewport's end leave nothing more to scroll to.
                    let lead = if rows.upper() > rows.page_size() {
                        above
                    } else {
                        let shown = list.child().map_or(0, |rows| {
                            rows.measure(gtk::Orientation::Vertical, rows.width()).1
                        });
                        (top(list.upcast_ref()) + shown.min(room) - height).clamp(0, above)
                    };
                    (above, lead)
                }
                _ => {
                    let lead = (content - height).max(0);
                    (lead, lead)
                }
            };
            let resized = self.above.replace(above) != above;
            let moved = self.lead.replace(lead) != lead;
            if self.sync_list() || resized || moved {
                place();
            }
            // The strip fades in with the scroll, over its own height, and is whole when the
            // header is under it, while what follows the header still leaves under it. A page
            // that ends sooner never gets that far.
            let whole = header_end - strip_height;
            let shown = if whole > 0 && whole <= lead {
                let start = (whole - strip_height).max(0);
                (f64::from(self.offset() - start) / f64::from(whole - start)).clamp(0.0, 1.0)
            } else {
                0.0
            };
            strip.set_opacity(shown);
            strip.set_can_target(shown >= 1.0);
            strip.allocate(width, strip_height, -1, None);
            self.covered.set(strip_height);
            self.update_range();
            let across = self.hadjustment.borrow().clone();
            if let Some(across) = across {
                let width = f64::from(width);
                across.configure(0.0, 0.0, width, width * 0.1, width * 0.9, width);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (Some(page), Some(strip)) =
                (self.page.borrow().clone(), self.strip.borrow().clone())
            else {
                return;
            };
            // What passes under the strip does not show around it once it has faded in.
            if strip.opacity() >= 1.0 {
                let under = self.covered.get() as f32;
                snapshot.push_clip(&gtk::graphene::Rect::new(
                    0.0,
                    under,
                    obj.width() as f32,
                    (obj.height() as f32 - under).max(0.0),
                ));
                obj.snapshot_child(&page, snapshot);
                snapshot.pop();
            } else {
                obj.snapshot_child(&page, snapshot);
            }
            obj.snapshot_child(&strip, snapshot);
        }
    }

    impl ScrollableImpl for DetailScroll {}

    impl DetailScroll {
        fn set_vadjustment(&self, adjustment: Option<gtk::Adjustment>) {
            let previous = self.vadjustment.replace(adjustment.clone());
            if let (Some(previous), Some(handler)) = (previous, self.scrolled.take()) {
                previous.disconnect(handler);
            }
            let Some(adjustment) = adjustment else {
                return;
            };
            let weak = self.obj().downgrade();
            let handler = adjustment.connect_value_changed(move |_| {
                if let Some(obj) = weak.upgrade() {
                    obj.imp().sync_list();
                    obj.queue_allocate();
                }
            });
            self.scrolled.replace(Some(handler));
            self.update_range();
        }

        fn position(&self) -> f64 {
            self.vadjustment
                .borrow()
                .as_ref()
                .map_or(0.0, |adjustment| adjustment.value())
        }

        /// How far the page has left.
        fn offset(&self) -> i32 {
            (self.position().round() as i32).clamp(0, self.lead.get())
        }

        /// The list scrolls by what the page scrolled past its lead. Returns whether that
        /// moved the list.
        fn sync_list(&self) -> bool {
            let Some(rows) = self.list.borrow().as_ref().map(|list| list.vadjustment()) else {
                return false;
            };
            let value = (self.position() - f64::from(self.lead.get())).max(0.0);
            if rows.value() == value {
                return false;
            }
            self.syncing.set(true);
            rows.set_value(value);
            self.syncing.set(false);
            true
        }

        /// Brings a focused row that is under the viewport's end into view: while the page
        /// has not left, the list's own end is below it.
        fn show_focus(&self, focus: Option<gtk::Widget>) {
            let (Some(focus), Some(frame), Some(adjustment)) = (
                focus,
                self.frame.borrow().clone(),
                self.vadjustment.borrow().clone(),
            ) else {
                return;
            };
            if !focus.is_ancestor(&frame) {
                return;
            }
            let Some(bounds) = focus.compute_bounds(&*self.obj()) else {
                return;
            };
            let under = f64::from(bounds.y() + bounds.height()) - f64::from(self.viewport.get());
            if under > 0.0 {
                adjustment.set_value(adjustment.value() + under.ceil());
            }
        }

        /// The list moved itself, to a row the keyboard or a search went to: the page goes
        /// past its lead with it.
        pub(super) fn list_moved(&self) {
            if self.syncing.get() {
                return;
            }
            let (Some(adjustment), Some(rows)) = (
                self.vadjustment.borrow().clone(),
                self.list.borrow().as_ref().map(|list| list.vadjustment()),
            ) else {
                return;
            };
            let lead = f64::from(self.lead.get());
            if rows.value() > 0.0 || adjustment.value() > lead {
                adjustment.set_value(lead + rows.value());
            }
        }

        /// The page scrolls through its lead, then through what the list holds below its end.
        pub(super) fn update_range(&self) {
            let Some(adjustment) = self.vadjustment.borrow().clone() else {
                return;
            };
            let viewport = f64::from(self.viewport.get());
            if viewport <= 0.0 {
                return;
            }
            let rows = self.list.borrow().as_ref().map(|list| list.vadjustment());
            let below = match rows {
                Some(rows) if self.pinned.get() => (rows.upper() - rows.page_size()).max(0.0),
                _ => 0.0,
            };
            let extent = f64::from(self.lead.get()) + below;
            adjustment.configure(
                adjustment.value().clamp(0.0, extent),
                0.0,
                viewport + extent,
                viewport * 0.1,
                viewport * 0.9,
                viewport,
            );
        }
    }

    /// Whether `widget` shows inside `page`: a stack's other pages do not.
    fn shows(widget: &gtk::Widget, page: &gtk::Widget) -> bool {
        let mut current = Some(widget.clone());
        while let Some(widget) = current {
            if &widget == page {
                return true;
            }
            if !widget.is_visible() || !widget.is_child_visible() {
                return false;
            }
            current = widget.parent();
        }
        false
    }
}

glib::wrapper! {
    pub struct DetailScroll(ObjectSubclass<imp::DetailScroll>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Scrollable;
}

/// Scrolls `page`, which starts with `header`; `strip` fades in at the top as `header` leaves.
/// With `list`, a list's scroller and its frame (the list with its column header) in `page`,
/// the page scrolls as one with that list: it leaves until the frame is under `strip`, and the
/// rows scroll on from there.
pub fn detail_scroll(
    page: &impl IsA<gtk::Widget>,
    header: &impl IsA<gtk::Widget>,
    strip: &impl IsA<gtk::Widget>,
    list: Option<(&gtk::ScrolledWindow, &gtk::Widget)>,
) -> gtk::Widget {
    let view: DetailScroll = glib::Object::new();
    view.set_overflow(gtk::Overflow::Hidden);
    let imp = view.imp();
    page.set_parent(&view);
    imp.page.replace(Some(page.clone().upcast()));
    strip.set_opacity(0.0);
    strip.set_can_target(false);
    strip.set_parent(&view);
    imp.strip.replace(Some(strip.clone().upcast()));
    imp.header.replace(Some(header.clone().upcast()));

    if let Some((list, frame)) = list {
        imp.frame.replace(Some(frame.clone()));
        // The page's scroller scrolls the list: the list's own stays its viewport, with no bar,
        // and leaves the wheel and touch to the page's.
        list.set_policy(gtk::PolicyType::External, gtk::PolicyType::External);
        list.set_kinetic_scrolling(false);
        for controller in &list.observe_controllers() {
            if let Ok(Ok(wheel)) =
                controller.map(|controller| controller.downcast::<gtk::EventControllerScroll>())
            {
                wheel.set_propagation_phase(gtk::PropagationPhase::None);
            }
        }
        let rows = list.vadjustment();
        let weak = view.downgrade();
        let moved = rows.connect_value_changed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.imp().list_moved();
            }
        });
        // More rows, or a list of another size, change how far the page scrolls.
        let weak = view.downgrade();
        let resized = rows.connect_changed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.queue_allocate();
            }
        });
        imp.list_handlers.replace(vec![moved, resized]);
        imp.list.replace(Some(list.clone()));
    }

    let scroller = gtk::ScrolledWindow::new();
    configure_library_route_scroller(&scroller);
    scroller.set_child(Some(&view));
    scroller.upcast()
}
