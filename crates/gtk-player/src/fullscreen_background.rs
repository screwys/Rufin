use gtk::{gdk, glib, graphene, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};
use std::time::Instant;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Background {
        pub current_paintable: RefCell<Option<gdk::Paintable>>,
        pub current_source: RefCell<Option<gdk::Texture>>,
        pub current_texture: RefCell<Option<gdk::Texture>>,
        pub previous_texture: RefCell<Option<gdk::Texture>>,
        pub current_color: Cell<[f32; 3]>,
        pub previous_color: Cell<[f32; 3]>,
        pub target_color: Cell<[f32; 3]>,
        pub start_time: Cell<Option<Instant>>,
        pub tick_id: RefCell<Option<gtk::TickCallbackId>>,
        pub image: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Background {
        const NAME: &'static str = "RufinFullscreenBackground";
        type Type = super::FullscreenBackground;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Background {
        fn dispose(&self) {
            if let Some(tick) = self.tick_id.borrow_mut().take() {
                tick.remove();
            }
        }
    }

    impl WidgetImpl for Background {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let (width, height) = (obj.width() as f32, obj.height() as f32);
            if width <= 0.0 || height <= 0.0 {
                return;
            }
            let bounds = graphene::Rect::new(0.0, 0.0, width, height);
            let dark = adw::StyleManager::default().is_dark();

            let t = if let Some(start) = self.start_time.get() {
                let elapsed = start.elapsed().as_secs_f32();
                let duration = 0.7; // 700ms smooth transition
                let p = (elapsed / duration).clamp(0.0, 1.0);
                if p >= 1.0 {
                    self.start_time.set(None);
                    self.previous_texture.borrow_mut().take();
                    self.previous_color.set(self.target_color.get());
                    1.0
                } else {
                    p * p * (3.0 - 2.0 * p) // smoothstep
                }
            } else {
                1.0
            };

            let [r0, g0, b0] = self.previous_color.get();
            let [r1, g1, b1] = self.target_color.get();
            let color = [r0 + (r1 - r0) * t, g0 + (g1 - g0) * t, b0 + (b1 - b0) * t];
            self.current_color.set(color);

            let tone = |c: f32| if dark { c * 0.22 } else { 0.85 + c * 0.15 };
            snapshot.append_color(
                &gdk::RGBA::new(tone(color[0]), tone(color[1]), tone(color[2]), 1.0),
                &bounds,
            );

            if self.image.get() {
                let base_opacity = if dark { 0.25 } else { 0.12 };
                if t < 1.0
                    && let Some(prev) = self.previous_texture.borrow().as_ref()
                {
                    draw_blurred_texture(
                        snapshot,
                        prev,
                        &bounds,
                        base_opacity * (1.0 - t),
                        width,
                        height,
                    );
                }
                if let Some(curr) = self.current_texture.borrow().as_ref() {
                    draw_blurred_texture(snapshot, curr, &bounds, base_opacity * t, width, height);
                }
            }
        }
    }

    fn draw_blurred_texture(
        snapshot: &gtk::Snapshot,
        texture: &gdk::Texture,
        bounds: &graphene::Rect,
        opacity: f32,
        width: f32,
        height: f32,
    ) {
        if opacity <= 0.001 {
            return;
        }
        let scale = (width / texture.width() as f32).max(height / texture.height() as f32);
        let (w, h) = (
            texture.width() as f32 * scale,
            texture.height() as f32 * scale,
        );
        snapshot.push_clip(bounds);
        snapshot.push_opacity(f64::from(opacity));
        snapshot.push_blur(24.0);
        snapshot.append_texture(
            texture,
            &graphene::Rect::new((width - w) / 2.0, (height - h) / 2.0, w, h),
        );
        snapshot.pop();
        snapshot.pop();
        snapshot.pop();
    }
}

glib::wrapper! {
    pub struct FullscreenBackground(ObjectSubclass<imp::Background>)
        @extends gtk::Widget, @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl FullscreenBackground {
    pub fn color(&self) -> [f32; 3] {
        self.imp().target_color.get()
    }

    pub fn new() -> Self {
        glib::Object::builder()
            .property("hexpand", true)
            .property("vexpand", true)
            .property("can-target", false)
            .build()
    }

    pub fn update(&self, paintable: Option<gdk::Paintable>, image: bool) {
        let imp = self.imp();
        let paintable_same = match (&*imp.current_paintable.borrow(), &paintable) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        let image_changed = imp.image.replace(image) != image;
        if paintable_same && !image_changed {
            return;
        }
        *imp.current_paintable.borrow_mut() = paintable.clone();
        let texture = paintable.and_then(|p| p.current_image().downcast::<gdk::Texture>().ok());
        if *imp.current_source.borrow() != texture {
            *imp.current_source.borrow_mut() = texture.clone();
            let mut color = [0.0; 3];
            let bg_texture = texture.clone();
            if let Some(texture) = &texture {
                let mut downloader = gdk::TextureDownloader::new(texture);
                downloader.set_format(gdk::MemoryFormat::R8g8b8a8);
                let (bytes, stride) = downloader.download_bytes();
                let mut count = 0.0;
                let tw = texture.width() as usize;
                let th = texture.height() as usize;
                for y in (0..th).step_by((th / 32).max(1)) {
                    for x in (0..tw).step_by((tw / 32).max(1)) {
                        let pixel = &bytes[y * stride + x * 4..][..4];
                        let alpha = f32::from(pixel[3]) / 255.0;
                        for channel in 0..3 {
                            color[channel] += f32::from(pixel[channel]) / 255.0 * alpha;
                        }
                        count += alpha;
                    }
                }
                if count > 0.0 {
                    for c in &mut color {
                        *c /= count;
                    }
                }
            }

            let target = imp.target_color.get();
            let color_distance = (color[0] - target[0]).abs()
                + (color[1] - target[1]).abs()
                + (color[2] - target[2]).abs();

            if imp.current_texture.borrow().is_none() || color_distance < 0.03 {
                // Initial appearance or identical track: apply immediately without transition
                imp.previous_color.set(color);
                imp.current_color.set(color);
                imp.target_color.set(color);
                *imp.current_texture.borrow_mut() = bg_texture;
                imp.previous_texture.borrow_mut().take();
                imp.start_time.set(None);
            } else {
                // Transition smoothly between songs
                imp.previous_color.set(imp.current_color.get());
                *imp.previous_texture.borrow_mut() = imp.current_texture.borrow().clone();
                imp.target_color.set(color);
                *imp.current_texture.borrow_mut() = bg_texture;
                imp.start_time.set(Some(Instant::now()));
                self.ensure_tick();
            }
        }
        imp.image.set(image);
        self.queue_draw();
    }

    fn ensure_tick(&self) {
        let imp = self.imp();
        if imp.tick_id.borrow().is_none() {
            let tick = self.add_tick_callback(|widget, _frame_clock| {
                let imp = widget.imp();
                if imp.start_time.get().is_some() {
                    widget.queue_draw();
                    glib::ControlFlow::Continue
                } else {
                    *imp.tick_id.borrow_mut() = None;
                    glib::ControlFlow::Break
                }
            });
            *imp.tick_id.borrow_mut() = Some(tick);
        }
    }
}
