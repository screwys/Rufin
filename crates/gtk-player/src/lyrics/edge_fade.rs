use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LyricsEdgeFade;

    #[glib::object_subclass]
    impl ObjectSubclass for LyricsEdgeFade {
        const NAME: &'static str = "RufinLyricsEdgeFade";
        type Type = super::LyricsEdgeFade;
        type ParentType = gtk::Box;
    }

    impl ObjectImpl for LyricsEdgeFade {}
    impl BoxImpl for LyricsEdgeFade {}

    impl WidgetImpl for LyricsEdgeFade {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let height = widget.height() as f32;
            if height <= 0.0 {
                return;
            }
            let edge = (24.0 / height).min(0.5);
            snapshot.push_mask(gtk::gsk::MaskMode::Alpha);
            snapshot.append_linear_gradient(
                &gtk::graphene::Rect::new(0.0, 0.0, widget.width() as f32, height),
                &gtk::graphene::Point::new(0.0, 0.0),
                &gtk::graphene::Point::new(0.0, height),
                &[
                    gtk::gsk::ColorStop::new(0.0, gtk::gdk::RGBA::TRANSPARENT),
                    gtk::gsk::ColorStop::new(edge, gtk::gdk::RGBA::WHITE),
                    gtk::gsk::ColorStop::new(1.0 - edge, gtk::gdk::RGBA::WHITE),
                    gtk::gsk::ColorStop::new(1.0, gtk::gdk::RGBA::TRANSPARENT),
                ],
            );
            snapshot.pop();
            self.parent_snapshot(snapshot);
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct LyricsEdgeFade(ObjectSubclass<imp::LyricsEdgeFade>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}
