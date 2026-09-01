use adw::prelude::*;

const LARGE_POPUP_HEIGHT_PERCENT: i32 = 85;
const LARGE_POPUP_WIDTH_NUMERATOR: i32 = 11;
const LARGE_POPUP_WIDTH_DENOMINATOR: i32 = 10;
pub(crate) const LARGE_POPUP_BASE_WIDTH: i32 = 700;
pub(crate) const LARGE_POPUP_BASE_HEIGHT: i32 = 640;

mod allocation_owner_imp {
    use std::{cell::RefCell, rc::Rc};

    use gtk::{glib, prelude::*, subclass::prelude::*};

    #[derive(Default)]
    pub struct AllocationOwner {
        pub(super) on_width: RefCell<Option<Rc<dyn Fn(i32)>>>,
        pub(super) on_size: RefCell<Option<Rc<dyn Fn(i32, i32)>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AllocationOwner {
        const NAME: &'static str = "RufinAllocationOwner";
        type Type = super::AllocationOwner;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for AllocationOwner {
        fn dispose(&self) {
            self.on_width.take();
            self.on_size.take();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for AllocationOwner {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            if self.on_width.borrow().is_some() {
                return gtk::SizeRequestMode::HeightForWidth;
            }
            self.obj()
                .first_child()
                .map(|child| child.request_mode())
                .unwrap_or(gtk::SizeRequestMode::ConstantSize)
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.obj().first_child() else {
                return (0, 0, -1, -1);
            };
            if self.on_width.borrow().is_some() {
                if orientation == gtk::Orientation::Horizontal {
                    // The stable parent grants this owner its width. Propagating
                    // the child's pre-responsive minimum would prevent GTK from
                    // ever measuring the narrower child state.
                    return (1, 1, -1, -1);
                }
                if for_size > 1 {
                    self.apply_width(for_size);
                }
            }
            child.measure(orientation, for_size)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.obj().first_child() else {
                return;
            };
            if width > 1 {
                self.apply_width(width);
                self.apply_size(width, height);
            }
            child.allocate(width, height, baseline, None);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.obj().first_child() {
                self.obj().snapshot_child(&child, snapshot);
            }
        }
    }

    impl AllocationOwner {
        fn apply_width(&self, width: i32) {
            let on_width = self.on_width.borrow().clone();
            if let Some(on_width) = on_width {
                on_width(width);
            }
        }

        fn apply_size(&self, width: i32, height: i32) {
            let on_size = self.on_size.borrow().clone();
            if let Some(on_size) = on_size {
                on_size(width, height);
            }
        }
    }
}

gtk::glib::wrapper! {
    pub struct AllocationOwner(ObjectSubclass<allocation_owner_imp::AllocationOwner>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AllocationOwner {
    pub(crate) fn set_width_callback(&self, on_width: impl Fn(i32) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;

        self.imp()
            .on_width
            .replace(Some(std::rc::Rc::new(on_width)));
    }

    pub(crate) fn set_size_callback(&self, on_size: impl Fn(i32, i32) + 'static) {
        use gtk::subclass::prelude::ObjectSubclassIsExt;

        self.imp().on_size.replace(Some(std::rc::Rc::new(on_size)));
    }
}

/// Applies width-dependent child state before height-for-width measurement and allocation without
/// propagating the pre-responsive child's horizontal minimum. Callbacks must ignore widths they
/// have already applied.
pub(crate) fn width_allocation_owner(
    child: &impl IsA<gtk::Widget>,
    on_width: impl Fn(i32) + 'static,
) -> AllocationOwner {
    let owner = allocation_owner_for_child(child);
    owner.set_width_callback(on_width);
    owner
}

/// Applies width-and-height-dependent child state before child allocation. Callbacks must ignore
/// allocation pairs they have already applied.
pub(crate) fn allocation_owner(
    child: &impl IsA<gtk::Widget>,
    on_size: impl Fn(i32, i32) + 'static,
) -> AllocationOwner {
    let owner = allocation_owner_for_child(child);
    owner.set_size_callback(on_size);
    owner
}

fn allocation_owner_for_child(child: &impl IsA<gtk::Widget>) -> AllocationOwner {
    let owner: AllocationOwner = gtk::glib::Object::new();
    owner.set_hexpand(child.hexpands());
    owner.set_vexpand(child.vexpands());
    owner.set_halign(child.halign());
    owner.set_valign(child.valign());
    owner.set_accessible_role(gtk::AccessibleRole::Presentation);
    child.set_parent(&owner);
    owner
}

pub(crate) fn large_popup_content_height(app_height: i32, fallback_height: i32) -> i32 {
    if app_height <= 0 {
        return fallback_height;
    }
    (app_height * LARGE_POPUP_HEIGHT_PERCENT + 50) / 100
}

pub(crate) fn large_popup_content_width(base_width: i32) -> i32 {
    (base_width * LARGE_POPUP_WIDTH_NUMERATOR + LARGE_POPUP_WIDTH_DENOMINATOR / 2)
        / LARGE_POPUP_WIDTH_DENOMINATOR
}

pub(crate) fn configure_fill_width_clip(
    scroller: &gtk::ScrolledWindow,
    vertical_policy: gtk::PolicyType,
) {
    scroller.set_policy(gtk::PolicyType::External, vertical_policy);
    scroller.set_overflow(gtk::Overflow::Hidden);
    scroller.set_width_request(1);
    scroller.set_min_content_width(0);
    scroller.set_max_content_width(1);
    scroller.set_propagate_natural_width(false);
    lock_horizontal_adjustment(scroller);
}

fn lock_horizontal_adjustment(scroller: &gtk::ScrolledWindow) {
    let adjustment = scroller.hadjustment();
    adjustment.connect_value_changed(|adjustment| {
        if adjustment.value() != 0.0 {
            adjustment.set_value(0.0);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{large_popup_content_height, large_popup_content_width};

    #[test]
    fn large_popup_dimensions_follow_shared_policy() {
        assert_eq!(large_popup_content_height(1_000, 640), 850);
        assert_eq!(large_popup_content_height(0, 640), 640);
        assert_eq!(large_popup_content_width(560), 616);
        assert_eq!(large_popup_content_width(620), 682);
    }
}
