use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gtk::{glib, prelude::*};

const PAUSE_SECONDS: f64 = 2.0;
const FONT_HEIGHTS_PER_SECOND: f64 = 1.1;

fn align_glyphs(label: &gtk::Label) {
    let context = label.pango_context();
    if !context.is_round_glyph_positions() {
        // Keep glyph bounds aligned with painted pixels while the viewport moves.
        context.set_round_glyph_positions(true);
        label.layout().context_changed();
        label.queue_resize();
    }
}

pub fn new_label() -> (gtk::Widget, gtk::Label) {
    let resource = crate::ui_resource::TEXT_SLIDE_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        fade: crate::edge_fade::TextFade,
        scroll: gtk::ScrolledWindow, viewport: gtk::Viewport, label: gtk::Label,
    });
    install(&scroll, &viewport, &label);
    (fade.upcast(), label)
}

struct Slide {
    scroll: glib::WeakRef<gtk::ScrolledWindow>,
    viewport: glib::WeakRef<gtk::Viewport>,
    label: glib::WeakRef<gtk::Label>,
    tick: RefCell<Option<gtk::TickCallbackId>>,
    started: Cell<Option<i64>>,
    completed: Cell<bool>,
    settings_handler: RefCell<Option<glib::SignalHandlerId>>,
    scroll_handlers: RefCell<Vec<(gtk::Adjustment, glib::SignalHandlerId)>>,
}

impl Slide {
    fn stop(&self) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
        self.started.set(None);
    }

    fn reset(self: &Rc<Self>) {
        self.stop();
        let (Some(scroll), Some(label)) = (self.scroll.upgrade(), self.label.upgrade()) else {
            return;
        };
        if label.has_focus() {
            return;
        }
        let adjustment = scroll.hadjustment();
        let overflow = (adjustment.upper() - adjustment.page_size()).max(0.0);
        let rtl = label.direction() == gtk::TextDirection::Rtl;
        let metrics = label.pango_context().metrics(None, None);
        let pixels_per_second =
            f64::from(metrics.height()) / f64::from(gtk::pango::SCALE) * FONT_HEIGHTS_PER_SECOND;
        adjustment.set_value(if rtl { overflow } else { 0.0 });
        if self.completed.get()
            || !scroll.is_mapped()
            || !scroll.settings().is_gtk_enable_animations()
            || overflow <= 1.0
        {
            return;
        }
        let mut parent = scroll.parent();
        while let Some(ancestor) = parent {
            if ancestor.is::<gtk::ScrolledWindow>()
                && let Some(bounds) = scroll.compute_bounds(&ancestor)
                && (bounds.y() + bounds.height() <= 0.0 || bounds.y() >= ancestor.height() as f32)
            {
                return;
            }
            parent = ancestor.parent();
        }
        let weak = Rc::downgrade(self);
        self.tick
            .replace(Some(scroll.add_tick_callback(move |scroll, clock| {
                let Some(state) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                if let Some(label) = state.label.upgrade() {
                    align_glyphs(&label);
                }
                let start = state.started.get().unwrap_or_else(|| {
                    state.started.set(Some(clock.frame_time()));
                    clock.frame_time()
                });
                let elapsed = (clock.frame_time() - start) as f64 / 1_000_000.0;
                let travel = overflow / pixels_per_second;
                if elapsed >= 2.0 * (PAUSE_SECONDS + travel) {
                    scroll
                        .hadjustment()
                        .set_value(if rtl { overflow } else { 0.0 });
                    state.completed.set(true);
                    state.started.set(None);
                    state.tick.take();
                    return glib::ControlFlow::Break;
                }
                let phase = elapsed;
                let offset = if phase < PAUSE_SECONDS {
                    0.0
                } else if phase < PAUSE_SECONDS + travel {
                    (phase - PAUSE_SECONDS) * pixels_per_second
                } else if phase < 2.0 * PAUSE_SECONDS + travel {
                    overflow
                } else {
                    overflow - (phase - 2.0 * PAUSE_SECONDS - travel) * pixels_per_second
                };
                scroll
                    .hadjustment()
                    .set_value(if rtl { overflow - offset } else { offset });
                glib::ControlFlow::Continue
            })));
    }

    fn sync(self: &Rc<Self>) {
        let (Some(scroll), Some(viewport), Some(label)) = (
            self.scroll.upgrade(),
            self.viewport.upgrade(),
            self.label.upgrade(),
        ) else {
            return;
        };
        let animate = scroll.settings().is_gtk_enable_animations();
        align_glyphs(&label);
        label.set_ellipsize(if animate {
            gtk::pango::EllipsizeMode::None
        } else {
            gtk::pango::EllipsizeMode::End
        });
        viewport.set_hscroll_policy(if animate {
            gtk::ScrollablePolicy::Natural
        } else {
            gtk::ScrollablePolicy::Minimum
        });
        scroll.set_tooltip_text(Some(&label.text()));
        self.reset();
    }
}

pub fn install(scroll: &gtk::ScrolledWindow, viewport: &gtk::Viewport, label: &gtk::Label) {
    let state = Rc::new(Slide {
        scroll: scroll.downgrade(),
        viewport: viewport.downgrade(),
        label: label.downgrade(),
        tick: RefCell::new(None),
        started: Cell::new(None),
        completed: Cell::new(false),
        settings_handler: RefCell::new(None),
        scroll_handlers: RefCell::new(Vec::new()),
    });
    let weak = Rc::downgrade(&state);
    scroll.hadjustment().connect_changed(move |_| {
        if let Some(state) = weak.upgrade() {
            state.reset();
        }
    });
    let weak = Rc::downgrade(&state);
    label.connect_label_notify(move |_| {
        if let Some(state) = weak.upgrade() {
            state.completed.set(false);
            state.sync();
        }
    });
    let motion = gtk::EventControllerMotion::new();
    let weak = Rc::downgrade(&state);
    motion.connect_enter(move |_, _, _| {
        if let Some(state) = weak.upgrade()
            && state.completed.replace(false)
        {
            state.reset();
        }
    });
    scroll.add_controller(motion);
    let weak = Rc::downgrade(&state);
    label.connect_has_focus_notify(move |_| {
        if let Some(state) = weak.upgrade() {
            state.reset();
        }
    });
    let mapped = Rc::clone(&state);
    scroll.connect_map(move |scroll| {
        let weak = Rc::downgrade(&mapped);
        mapped.settings_handler.replace(Some(
            scroll
                .settings()
                .connect_gtk_enable_animations_notify(move |_| {
                    if let Some(state) = weak.upgrade() {
                        state.sync();
                    }
                }),
        ));
        let mut parent = scroll.parent();
        while let Some(ancestor) = parent {
            if let Some(container) = ancestor.downcast_ref::<gtk::ScrolledWindow>() {
                let adjustment = container.vadjustment();
                let weak = Rc::downgrade(&mapped);
                let handler = adjustment.connect_value_changed(move |_| {
                    if let Some(state) = weak.upgrade() {
                        state.reset();
                    }
                });
                mapped
                    .scroll_handlers
                    .borrow_mut()
                    .push((adjustment, handler));
            }
            parent = ancestor.parent();
        }
        mapped.sync();
    });
    let unmapped = Rc::clone(&state);
    scroll.connect_unmap(move |scroll| {
        unmapped.stop();
        if let Some(handler) = unmapped.settings_handler.take() {
            scroll.settings().disconnect(handler);
        }
        for (adjustment, handler) in unmapped.scroll_handlers.take() {
            adjustment.disconnect(handler);
        }
    });
    state.sync();
}
