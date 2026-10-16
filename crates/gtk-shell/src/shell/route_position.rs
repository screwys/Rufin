use std::collections::{HashMap, VecDeque};

use gtk::prelude::*;

use gtk_widgets::mounted_route::{ItemScrollPosition, NativeListScroll};
use gtk_widgets::route::Route;

const ROUTE_POSITION_CAPACITY: usize = 64;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct RoutePositionKey {
    route: Route,
}

impl RoutePositionKey {
    pub(super) fn new(route: Route) -> Self {
        Self { route }
    }
}

pub(super) struct RoutePositionMemory {
    capacity: usize,
    positions: HashMap<RoutePositionKey, RoutePosition>,
    recency: VecDeque<RoutePositionKey>,
}

impl Default for RoutePositionMemory {
    fn default() -> Self {
        Self::with_capacity(ROUTE_POSITION_CAPACITY)
    }
}

impl RoutePositionMemory {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity,
            positions: HashMap::new(),
            recency: VecDeque::new(),
        }
    }

    pub(super) fn record(&mut self, key: RoutePositionKey, position: RoutePosition) {
        if self.capacity == 0 {
            return;
        }

        self.recency.retain(|candidate| candidate != &key);
        self.positions.insert(key.clone(), position);
        self.recency.push_back(key);
        while self.positions.len() > self.capacity {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            self.positions.remove(&oldest);
        }
    }

    pub(super) fn restore(&mut self, key: &RoutePositionKey) -> Option<RoutePosition> {
        let position = self.positions.get(key).copied()?;
        self.recency.retain(|candidate| candidate != key);
        self.recency.push_back(key.clone());
        Some(position)
    }

    pub(super) fn clear(&mut self) {
        self.positions.clear();
        self.recency.clear();
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RoutePosition {
    pub(super) value: f64,
    item: Option<ItemScrollPosition>,
    upper: f64,
    page_size: f64,
}

impl RoutePosition {
    pub(super) fn capture(adjustment: &gtk::Adjustment, items: Option<&NativeListScroll>) -> Self {
        Self {
            value: adjustment.value(),
            upper: adjustment.upper(),
            page_size: adjustment.page_size(),
            item: items.and_then(NativeListScroll::capture),
        }
    }

    pub(super) fn window(self) -> library::RouteSeedWindow {
        self.item.and_then(|item| item.query_position).map_or_else(
            || library::RouteSeedWindow::relative(self.relative()),
            library::RouteSeedWindow::position,
        )
    }

    pub(super) fn relative(self) -> f64 {
        let scrollable = (self.upper - self.page_size).max(0.0);
        if scrollable <= f64::EPSILON {
            0.0
        } else {
            (self.value / scrollable).clamp(0.0, 1.0)
        }
    }
}

pub(super) fn clamp_route_position(position: f64, lower: f64, upper: f64, page_size: f64) -> f64 {
    position.clamp(lower, (upper - page_size).max(lower))
}

mod surface_imp {
    use std::cell::{Cell, RefCell};

    use gtk::{glib, prelude::*, subclass::prelude::*};

    #[derive(Default)]
    pub struct RouteSurface {
        pub(super) adjustment: RefCell<Option<gtk::Adjustment>>,
        pub(super) target: Cell<Option<super::RoutePosition>>,
        pub(super) items: RefCell<Option<super::NativeListScroll>>,
        pub(super) allocated: RefCell<Option<Box<dyn Fn()>>>,
        pub(super) readiness: RefCell<Option<std::rc::Rc<super::VisibleContent>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RouteSurface {
        const NAME: &'static str = "RufinRouteSurface";
        type Type = super::RouteSurface;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for RouteSurface {
        fn dispose(&self) {
            self.adjustment.take();
            self.target.take();
            self.items.take();
            self.allocated.take();
            self.readiness.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RouteSurface {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.obj()
                .first_child()
                .map(|child| child.request_mode())
                .unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.obj()
                .first_child()
                .map(|child| child.measure(orientation, for_size))
                .unwrap_or((0, 0, -1, -1))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            child.allocate(width, height, baseline, None);

            if width <= 1 || height <= 1 {
                return;
            }
            if let (Some(target), Some(adjustment)) = (self.target.take(), self.adjustment.take()) {
                if let (Some(item), Some(items)) = (target.item, self.items.take()) {
                    items.scroll_to(item.position);
                    child.allocate(width, height, baseline, None);
                    items.restore_offset(item);
                } else {
                    adjustment.set_value(super::clamp_route_position(
                        target.value,
                        adjustment.lower(),
                        adjustment.upper(),
                        adjustment.page_size(),
                    ));
                }
                // Readiness must inspect the restored viewport after native layout.
                child.allocate(width, height, baseline, None);
            }
            if let Some(allocated) = self.allocated.take() {
                allocated();
            }
            if let Some(readiness) = self.readiness.borrow().clone() {
                readiness.check();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
}

gtk::glib::wrapper! {
    pub struct RouteSurface(
        ObjectSubclass<surface_imp::RouteSurface>
    )
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub(super) fn mount_route_surface(
    child: &impl IsA<gtk::Widget>,
    adjustment: Option<&gtk::Adjustment>,
    target: Option<RoutePosition>,
    items: Option<NativeListScroll>,
    allocated: impl Fn() + 'static,
    ready: impl Fn(&gtk::Widget) + 'static,
) -> RouteSurface {
    use gtk::subclass::prelude::ObjectSubclassIsExt;

    let owner: RouteSurface = gtk::glib::Object::new();
    owner.set_hexpand(child.hexpands());
    owner.set_vexpand(child.vexpands());
    owner.set_halign(child.halign());
    owner.set_valign(child.valign());
    owner.set_accessible_role(gtk::AccessibleRole::Presentation);
    owner.imp().adjustment.replace(adjustment.cloned());
    owner.imp().target.set(target);
    owner.imp().items.replace(items);
    owner.imp().allocated.replace(Some(Box::new(allocated)));
    let weak = owner.downgrade();
    owner
        .imp()
        .readiness
        .replace(Some(VisibleContent::new(owner.upcast_ref(), move || {
            if let Some(owner) = weak.upgrade() {
                ready(owner.upcast_ref());
            }
        })));
    child.set_parent(&owner);
    owner
}

impl RouteSurface {
    pub(super) fn is_ready(&self) -> bool {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        self.imp()
            .readiness
            .borrow()
            .as_ref()
            .is_some_and(|readiness| readiness.is_ready())
    }
}

pub(super) struct VisibleContent {
    root: gtk::glib::WeakRef<gtk::Widget>,
    ready: std::cell::RefCell<Option<Box<dyn Fn()>>>,
    pending: std::cell::RefCell<Vec<(gtk::glib::WeakRef<gtk::Widget>, gtk::glib::SignalHandlerId)>>,
}

impl VisibleContent {
    pub(super) fn new(root: &gtk::Widget, ready: impl Fn() + 'static) -> std::rc::Rc<Self> {
        std::rc::Rc::new(Self {
            root: root.downgrade(),
            ready: std::cell::RefCell::new(Some(Box::new(ready))),
            pending: Default::default(),
        })
    }

    pub(super) fn is_ready(&self) -> bool {
        self.ready.borrow().is_none()
    }

    fn clear_pending(&self) {
        for (widget, handler) in self.pending.take() {
            if let Some(widget) = widget.upgrade() {
                widget.disconnect(handler);
            }
        }
    }

    fn pending_changed(&self) {
        if !self.pending.borrow().iter().any(|(widget, _)| {
            widget
                .upgrade()
                .is_some_and(|widget| presentation_pending(&widget))
        }) && let Some(root) = self.root.upgrade()
        {
            root.queue_allocate();
        }
    }

    pub(super) fn check(self: &std::rc::Rc<Self>) {
        if self.is_ready() {
            return;
        }
        let Some(root) = self
            .root
            .upgrade()
            .filter(|root| root.width() > 1 && root.height() > 1)
        else {
            return;
        };
        self.clear_pending();
        let mut pending = Vec::new();
        let clip = gtk::graphene::Rect::new(0.0, 0.0, root.width() as f32, root.height() as f32);
        visible_pending(&root, &root, &clip, &mut pending);
        if pending.is_empty() {
            if let Some(ready) = self.ready.take() {
                ready();
            }
        } else {
            self.pending.replace(
                pending
                    .into_iter()
                    .map(|widget| {
                        let owner = std::rc::Rc::downgrade(self);
                        let handler = widget.connect_css_classes_notify(move |_| {
                            if let Some(owner) = owner.upgrade() {
                                owner.pending_changed();
                            }
                        });
                        (widget.downgrade(), handler)
                    })
                    .collect(),
            );
        }
    }
}

impl Drop for VisibleContent {
    fn drop(&mut self) {
        self.clear_pending();
    }
}

// Row data must be ready. Artwork tiles can display a placeholder while loading.
fn presentation_pending(widget: &gtk::Widget) -> bool {
    widget.has_css_class("row-pending") || widget.has_css_class("track-skeleton")
}

fn visible_pending(
    widget: &gtk::Widget,
    root: &gtk::Widget,
    clip: &gtk::graphene::Rect,
    pending: &mut Vec<gtk::Widget>,
) {
    if !widget.is_visible() || !widget.is_child_visible() {
        return;
    }
    let Some(bounds) = widget.compute_bounds(root) else {
        return;
    };
    let Some(intersection) = bounds.intersection(clip) else {
        return;
    };
    if presentation_pending(widget) {
        pending.push(widget.clone());
        return;
    }
    let clip = if widget.overflow() == gtk::Overflow::Hidden || widget.is::<gtk::ScrolledWindow>() {
        &intersection
    } else {
        clip
    };
    let mut child = widget.first_child();
    while let Some(current) = child {
        child = current.next_sibling();
        visible_pending(&current, root, clip, pending);
    }
}

#[cfg(test)]
mod tests {
    use gtk_widgets::route::Route;

    use super::{RoutePosition, RoutePositionKey, RoutePositionMemory, clamp_route_position};

    #[test]
    fn position_memory_is_bounded_and_clears_with_the_source() {
        let home = RoutePositionKey::new(Route::Home);
        let tracks = RoutePositionKey::new(Route::Tracks);
        let albums = RoutePositionKey::new(Route::Albums);
        let mut memory = RoutePositionMemory::with_capacity(2);

        memory.record(
            home.clone(),
            RoutePosition {
                value: 120.0,
                upper: 1_000.0,
                page_size: 400.0,
                item: None,
            },
        );
        memory.record(
            tracks.clone(),
            RoutePosition {
                value: 240.0,
                upper: 1_000.0,
                page_size: 400.0,
                item: None,
            },
        );
        memory.record(
            home.clone(),
            RoutePosition {
                value: 180.0,
                upper: 1_000.0,
                page_size: 400.0,
                item: None,
            },
        );
        memory.record(
            albums.clone(),
            RoutePosition {
                value: 360.0,
                upper: 1_000.0,
                page_size: 400.0,
                item: None,
            },
        );

        assert_eq!(
            memory.restore(&home).map(|position| position.value),
            Some(180.0)
        );
        assert_eq!(memory.restore(&tracks), None);
        assert_eq!(
            memory.restore(&albums).map(|position| position.value),
            Some(360.0)
        );

        memory.clear();

        assert_eq!(memory.restore(&home), None);
        assert_eq!(memory.restore(&albums), None);
    }

    #[test]
    fn restored_position_is_clamped_to_the_current_scrollable_range() {
        assert_eq!(clamp_route_position(460.0, 0.0, 1_000.0, 400.0), 460.0);
        assert_eq!(clamp_route_position(900.0, 0.0, 1_000.0, 400.0), 600.0);
        assert_eq!(clamp_route_position(90.0, 100.0, 80.0, 20.0), 100.0);
    }

    #[test]
    fn restored_position_retains_its_relative_extent() {
        let position = super::RoutePosition {
            value: 300.0,
            item: None,
            upper: 1_000.0,
            page_size: 400.0,
        };
        assert_eq!(position.relative(), 0.5);
    }
}
