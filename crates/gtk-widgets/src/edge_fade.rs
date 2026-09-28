use gtk::{glib, prelude::*, subclass::prelude::*};

pub fn push_mask(
    snapshot: &gtk::Snapshot,
    width: f32,
    height: f32,
    vertical: bool,
    start: f32,
    end: f32,
) {
    let extent = if vertical { height } else { width };
    let start = (start / extent).min(0.5);
    let end = (end / extent).min(0.5);
    snapshot.push_mask(gtk::gsk::MaskMode::Alpha);
    snapshot.append_linear_gradient(
        &gtk::graphene::Rect::new(0.0, 0.0, width, height),
        &gtk::graphene::Point::new(0.0, 0.0),
        &gtk::graphene::Point::new(
            if vertical { 0.0 } else { width },
            if vertical { height } else { 0.0 },
        ),
        &[
            gtk::gsk::ColorStop::new(
                0.0,
                if start > 0.0 {
                    gtk::gdk::RGBA::TRANSPARENT
                } else {
                    gtk::gdk::RGBA::WHITE
                },
            ),
            gtk::gsk::ColorStop::new(start, gtk::gdk::RGBA::WHITE),
            gtk::gsk::ColorStop::new(1.0 - end, gtk::gdk::RGBA::WHITE),
            gtk::gsk::ColorStop::new(
                1.0,
                if end > 0.0 {
                    gtk::gdk::RGBA::TRANSPARENT
                } else {
                    gtk::gdk::RGBA::WHITE
                },
            ),
        ],
    );
    snapshot.pop();
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TextFade;

    #[glib::object_subclass]
    impl ObjectSubclass for TextFade {
        const NAME: &'static str = "RufinTextFade";
        type Type = super::TextFade;
        type ParentType = gtk::Box;
    }
    impl ObjectImpl for TextFade {}
    impl BoxImpl for TextFade {}
    impl WidgetImpl for TextFade {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let width = widget.width() as f32;
            let height = widget.height() as f32;
            let scroll = widget
                .first_child()
                .and_downcast::<gtk::ScrolledWindow>()
                .expect("Metadata fade contains its scroller");
            let adjustment = scroll.hadjustment();
            let left = adjustment.value().max(0.0).min(12.0) as f32;
            let right = (adjustment.upper() - adjustment.page_size() - adjustment.value())
                .max(0.0)
                .min(12.0) as f32;
            let masked = width > 0.0
                && height > 0.0
                && (left > 0.0 || right > 0.0)
                && widget.settings().is_gtk_enable_animations();
            if masked {
                push_mask(snapshot, width, height, false, left, right);
            }
            self.parent_snapshot(snapshot);
            if masked {
                snapshot.pop();
            }
        }
    }
}

glib::wrapper! {
    pub struct TextFade(ObjectSubclass<imp::TextFade>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}
