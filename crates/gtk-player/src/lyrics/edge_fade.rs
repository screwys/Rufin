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
            gtk_widgets::edge_fade::push_mask(
                snapshot,
                widget.width() as f32,
                height,
                true,
                24.0,
                24.0,
            );
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
