use std::{cell::RefCell, time::Duration};

use gtk::{glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PlayingIndicator {
        pub timer: RefCell<Option<glib::SourceId>>,
        settings_handler: RefCell<Option<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayingIndicator {
        const NAME: &'static str = "RufinPlayingIndicator";
        type Type = super::PlayingIndicator;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.set_css_name("playing-indicator");
            class.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
    }

    impl ObjectImpl for PlayingIndicator {}

    impl WidgetImpl for PlayingIndicator {
        fn measure(&self, orientation: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
            let size = if orientation == gtk::Orientation::Horizontal {
                13
            } else {
                12
            };
            (size, size, -1, -1)
        }

        fn map(&self) {
            self.parent_map();
            let obj = self.obj();
            let weak = obj.downgrade();
            self.settings_handler.replace(Some(
                obj.settings()
                    .connect_gtk_enable_animations_notify(move |_| {
                        if let Some(obj) = weak.upgrade() {
                            obj.sync_animation();
                        }
                    }),
            ));
            obj.sync_animation();
        }

        fn unmap(&self) {
            if let Some(timer) = self.timer.take() {
                timer.remove();
            }
            if let Some(handler) = self.settings_handler.take() {
                self.obj().settings().disconnect(handler);
            }
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let animations = obj.settings().is_gtk_enable_animations();
            let time = if animations {
                obj.frame_clock()
                    .map_or(0.0, |clock| clock.frame_time() as f64 / 1_000_000.0)
            } else {
                0.0
            };
            let color = obj.color();
            for index in 0..3 {
                let height = if animations {
                    let phase = ((time + f64::from(index) * 0.3) / 0.9) % 2.0;
                    let progress = if phase <= 1.0 { phase } else { 2.0 - phase };
                    let (start, end, progress) = if progress <= 0.5 {
                        (4.0, 12.0, progress * 2.0)
                    } else {
                        (12.0, 8.0, (progress - 0.5) * 2.0)
                    };
                    let eased = (1.0 - (progress * std::f64::consts::PI).cos()) * 0.5;
                    (start + (end - start) * eased) as f32
                } else {
                    6.0
                };
                let column = if obj.direction() == gtk::TextDirection::Rtl {
                    2 - index
                } else {
                    index
                };
                let rect =
                    gtk::graphene::Rect::new(column as f32 * 5.0, 12.0 - height, 3.0, height);
                snapshot.push_rounded_clip(&gtk::gsk::RoundedRect::from_rect(rect, 1.0));
                snapshot.append_color(&color, &rect);
                snapshot.pop();
            }
        }
    }
}

glib::wrapper! {
    pub struct PlayingIndicator(ObjectSubclass<imp::PlayingIndicator>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PlayingIndicator {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("can-target", false)
            .property("valign", gtk::Align::Center)
            .build()
    }

    fn sync_animation(&self) {
        if self.settings().is_gtk_enable_animations() {
            if self.imp().timer.borrow().is_none() {
                let weak = self.downgrade();
                self.imp().timer.replace(Some(glib::timeout_add_local(
                    Duration::from_millis(33),
                    move || {
                        let Some(widget) = weak.upgrade() else {
                            return glib::ControlFlow::Break;
                        };
                        widget.queue_draw();
                        glib::ControlFlow::Continue
                    },
                )));
            }
        } else if let Some(timer) = self.imp().timer.take() {
            timer.remove();
        }
        self.queue_draw();
    }
}
