use std::{cell::RefCell, time::Duration};

use gtk::{glib, prelude::*, subclass::prelude::*};

/// Mapped indicators share one timer so a step redraws all of them in one frame.
#[derive(Default)]
struct Animation {
    indicators: Vec<PlayingIndicator>,
    timer: Option<glib::SourceId>,
}

thread_local! {
    static ANIMATION: RefCell<Animation> = RefCell::default();
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct PlayingIndicator;

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
            let obj = self.obj().clone();
            ANIMATION.with_borrow_mut(|animation| {
                animation.indicators.push(obj);
                animation.timer.get_or_insert_with(|| {
                    glib::timeout_add_local(Duration::from_millis(33), || {
                        ANIMATION.with_borrow(|animation| {
                            animation.indicators.iter().for_each(WidgetExt::queue_draw);
                        });
                        glib::ControlFlow::Continue
                    })
                });
            });
        }

        fn unmap(&self) {
            let obj = self.obj();
            ANIMATION.with_borrow_mut(|animation| {
                animation.indicators.retain(|indicator| indicator != &*obj);
                if animation.indicators.is_empty()
                    && let Some(timer) = animation.timer.take()
                {
                    timer.remove();
                }
            });
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let time = obj
                .frame_clock()
                .map_or(0.0, |clock| clock.frame_time() as f64 / 1_000_000.0);
            let color = obj.color();
            for index in 0..3 {
                let phase = ((time + f64::from(index) * 0.3) / 0.9) % 2.0;
                let progress = if phase <= 1.0 { phase } else { 2.0 - phase };
                let (start, end, progress) = if progress <= 0.5 {
                    (4.0, 12.0, progress * 2.0)
                } else {
                    (12.0, 8.0, (progress - 0.5) * 2.0)
                };
                let eased = (1.0 - (progress * std::f64::consts::PI).cos()) * 0.5;
                let height = (start + (end - start) * eased) as f32;
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
        let indicator: Self = glib::Object::builder()
            .property("can-target", false)
            .property("valign", gtk::Align::Center)
            .build();
        indicator
            .settings()
            .bind_property("gtk-enable-animations", &indicator, "visible")
            .sync_create()
            .build();
        indicator
    }
}
