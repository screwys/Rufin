use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;
    use std::{cell::RefCell, ops::Range};

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/lyrics_snippet.ui")]
    pub struct LyricsSnippetView {
        #[template_child]
        pub label: gtk::TemplateChild<gtk::Label>,
        pub matches: RefCell<Vec<Range<u32>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LyricsSnippetView {
        const NAME: &'static str = "RufinLyricsSnippetView";
        type Type = super::LyricsSnippetView;
        type ParentType = gtk::Box;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for LyricsSnippetView {
        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for LyricsSnippetView {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let layout = self.label.layout().copy();
            let attrs = layout
                .attributes()
                .and_then(|attrs| attrs.copy())
                .unwrap_or_default();
            let accent = self.obj().color();
            for range in self.matches.borrow().iter() {
                let mut color = gtk::pango::AttrColor::new_foreground(
                    (accent.red() * u16::MAX as f32) as u16,
                    (accent.green() * u16::MAX as f32) as u16,
                    (accent.blue() * u16::MAX as f32) as u16,
                );
                color.set_start_index(range.start);
                color.set_end_index(range.end);
                attrs.change(color);
            }
            layout.set_attributes(Some(&attrs));
            let (x, y) = self.label.layout_offsets();
            let origin = self
                .label
                .compute_point(&*self.obj(), &gtk::graphene::Point::new(x as f32, y as f32))
                .expect("snippet label belongs to its view");
            snapshot.save();
            snapshot.translate(&origin);
            snapshot.append_layout(&layout, &self.label.color());
            snapshot.restore();
        }
    }
    impl BoxImpl for LyricsSnippetView {}
}

glib::wrapper! {
    pub struct LyricsSnippetView(ObjectSubclass<imp::LyricsSnippetView>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl LyricsSnippetView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn bind(&self, snippet: Option<&library::LyricsSnippet>) {
        let mut text = String::new();
        let mut matches = Vec::new();
        for part in snippet.into_iter().flat_map(|snippet| &snippet.parts) {
            let start = text.len() as u32;
            text.push_str(&part.text);
            if part.highlighted {
                matches.push(start..text.len() as u32);
            }
        }
        self.imp().matches.replace(matches);
        self.imp().label.set_text(&text);
        self.set_visible(!text.is_empty());
        self.queue_draw();
    }
}
