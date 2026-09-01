use std::collections::{HashMap, VecDeque};

use gtk::prelude::*;

use crate::routes::route::Route;

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

    pub(super) fn record(&mut self, key: RoutePositionKey, value: f64, upper: f64, page_size: f64) {
        if self.capacity == 0 || !value.is_finite() || !upper.is_finite() || !page_size.is_finite()
        {
            return;
        }

        self.recency.retain(|candidate| candidate != &key);
        self.positions.insert(
            key.clone(),
            RoutePosition {
                value,
                upper,
                page_size,
            },
        );
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
    upper: f64,
    page_size: f64,
}

impl RoutePosition {
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

mod restore_owner_imp {
    use std::cell::{Cell, RefCell};

    use gtk::{glib, prelude::*, subclass::prelude::*};

    #[derive(Default)]
    pub struct RoutePositionRestoreOwner {
        pub(super) adjustment: RefCell<Option<gtk::Adjustment>>,
        pub(super) target: Cell<Option<f64>>,
        pub(super) ready: RefCell<Option<std::rc::Rc<dyn Fn()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RoutePositionRestoreOwner {
        const NAME: &'static str = "RufinRoutePositionRestoreOwner";
        type Type = super::RoutePositionRestoreOwner;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for RoutePositionRestoreOwner {
        fn dispose(&self) {
            self.adjustment.take();
            self.target.take();
            self.ready.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RoutePositionRestoreOwner {
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
            let Some(target) = self.target.take() else {
                self.adjustment.take();
                if let Some(ready) = self.ready.take() {
                    ready();
                }
                return;
            };
            let Some(adjustment) = self.adjustment.take() else {
                return;
            };
            adjustment.set_value(super::clamp_route_position(
                target,
                adjustment.lower(),
                adjustment.upper(),
                adjustment.page_size(),
            ));
            self.obj().queue_allocate();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }
}

gtk::glib::wrapper! {
    pub struct RoutePositionRestoreOwner(
        ObjectSubclass<restore_owner_imp::RoutePositionRestoreOwner>
    )
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

pub(super) fn prepare_route_position_before_snapshot(
    child: &impl IsA<gtk::Widget>,
    adjustment: Option<&gtk::Adjustment>,
    target: Option<f64>,
    ready: impl Fn() + 'static,
) -> gtk::Widget {
    use gtk::subclass::prelude::ObjectSubclassIsExt;

    let owner: RoutePositionRestoreOwner = gtk::glib::Object::new();
    owner.set_hexpand(child.hexpands());
    owner.set_vexpand(child.vexpands());
    owner.set_halign(child.halign());
    owner.set_valign(child.valign());
    owner.set_accessible_role(gtk::AccessibleRole::Presentation);
    owner.imp().adjustment.replace(adjustment.cloned());
    owner.imp().target.set(target);
    owner.imp().ready.replace(Some(std::rc::Rc::new(ready)));
    child.set_parent(&owner);
    owner.upcast()
}

#[cfg(test)]
mod tests {
    use crate::routes::route::Route;

    use super::{RoutePositionKey, RoutePositionMemory, clamp_route_position};

    #[test]
    fn position_memory_is_bounded_and_clears_with_the_source() {
        let home = RoutePositionKey::new(Route::Home);
        let tracks = RoutePositionKey::new(Route::Tracks);
        let albums = RoutePositionKey::new(Route::Albums);
        let mut memory = RoutePositionMemory::with_capacity(2);

        memory.record(home.clone(), 120.0, 1_000.0, 400.0);
        memory.record(tracks.clone(), 240.0, 1_000.0, 400.0);
        memory.record(home.clone(), 180.0, 1_000.0, 400.0);
        memory.record(albums.clone(), 360.0, 1_000.0, 400.0);

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
            upper: 1_000.0,
            page_size: 400.0,
        };
        assert_eq!(position.relative(), 0.5);
    }
}
