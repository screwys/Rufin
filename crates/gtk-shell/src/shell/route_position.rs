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
            if let Some(adjustment) = self.adjustment.take() {
                // GTK anchors a new sectioned list to its first row, below the header.
                let target = self.target.take();
                if let (Some(item), Some(items)) =
                    (target.and_then(|target| target.item), self.items.take())
                {
                    items.scroll_to(item.position);
                    child.allocate(width, height, baseline, None);
                    items.restore_offset(item);
                } else {
                    adjustment.set_value(super::clamp_route_position(
                        target.map_or(adjustment.lower(), |target| target.value),
                        adjustment.lower(),
                        adjustment.upper(),
                        adjustment.page_size(),
                    ));
                }
                // Apply the restored viewport before resuming row reads.
                child.allocate(width, height, baseline, None);
            }
            if let Some(allocated) = self.allocated.take() {
                allocated();
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
    child.set_parent(&owner);
    owner
}

#[cfg(test)]
mod tests {
    use gtk_widgets::route::Route;

    use super::{RoutePosition, RoutePositionKey, RoutePositionMemory, clamp_route_position};

    #[test]
    fn position_memory_is_bounded_and_keeps_other_routes() {
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
