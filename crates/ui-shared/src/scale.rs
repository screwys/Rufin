use gtk::{glib, prelude::*};
use std::rc::Rc;
const SCALE_SURFACE_SCROLL_FACTOR: f64 = 2.5;
pub fn install_scale_scroll_forwarding(scale: &gtk::Scale) {
    let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    controller.set_propagation_phase(gtk::PropagationPhase::Capture);
    let scale_weak = scale.downgrade();
    controller.connect_scroll(move |controller, _, dy| {
        if dy == 0.0 {
            return gtk::glib::Propagation::Proceed;
        }
        let Some(scale) = scale_weak.upgrade() else {
            return gtk::glib::Propagation::Stop;
        };
        if let Some(scroller) = nearest_parent_scroller(scale.upcast_ref()) {
            let adjustment = scroller.vadjustment();
            adjustment.set_value(scale_scroll_target(
                adjustment.value(),
                adjustment.lower(),
                adjustment.upper(),
                adjustment.page_size(),
                dy,
                controller.unit() == gtk::gdk::ScrollUnit::Surface,
            ));
        }
        gtk::glib::Propagation::Stop
    });
    scale.add_controller(controller);
}

fn nearest_parent_scroller(widget: &gtk::Widget) -> Option<gtk::ScrolledWindow> {
    let mut parent = widget.parent();
    while let Some(widget) = parent {
        if let Ok(scroller) = widget.clone().downcast::<gtk::ScrolledWindow>() {
            return Some(scroller);
        }
        parent = widget.parent();
    }
    None
}

fn scale_scroll_target(
    value: f64,
    lower: f64,
    upper: f64,
    page_size: f64,
    dy: f64,
    surface_units: bool,
) -> f64 {
    let multiplier = if surface_units {
        SCALE_SURFACE_SCROLL_FACTOR
    } else {
        page_size.powf(2.0 / 3.0)
    };
    (value + dy * multiplier).clamp(lower, (upper - page_size).max(lower))
}

pub fn install_sliding_value_bubble(
    scale: &gtk::Scale,
    format_value: impl Fn(f64) -> String + 'static,
) {
    scale.set_draw_value(false);
    let format_value: Rc<dyn Fn(f64) -> String> = Rc::new(format_value);
    let value = gtk::Label::new(Some(&format_value(scale.value())));
    value.set_halign(gtk::Align::Center);
    value.set_valign(gtk::Align::Center);
    value.set_xalign(0.5);
    value.set_yalign(0.5);
    let bubble = gtk::Popover::new();
    bubble.add_css_class("slider-value-popover");
    bubble.set_autohide(false);
    bubble.set_focusable(false);
    bubble.set_has_arrow(false);
    bubble.set_position(gtk::PositionType::Top);
    bubble.set_offset(0, -4);
    bubble.set_child(Some(&value));
    bubble.set_parent(scale);

    let value_for_change = value.clone();
    let bubble_for_change = bubble.clone();
    let format_for_change = Rc::clone(&format_value);
    scale.connect_value_changed(move |scale| {
        value_for_change.set_text(&format_for_change(scale.value()));
        point_value_bubble_at_slider(scale, &bubble_for_change);
    });

    let events = gtk::EventControllerLegacy::new();
    events.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak_scale = scale.downgrade();
    let value_for_event = value.clone();
    let bubble_for_event = bubble.clone();
    let format_for_event = Rc::clone(&format_value);
    events.connect_event(move |_, event| {
        let visible = match event.event_type() {
            gtk::gdk::EventType::ButtonPress
            | gtk::gdk::EventType::TouchBegin
            | gtk::gdk::EventType::KeyPress => Some(true),
            gtk::gdk::EventType::ButtonRelease
            | gtk::gdk::EventType::TouchEnd
            | gtk::gdk::EventType::TouchCancel
            | gtk::gdk::EventType::KeyRelease
            | gtk::gdk::EventType::GrabBroken => Some(false),
            _ => None,
        };
        if let Some(visible) = visible {
            if visible {
                if let Some(scale) = weak_scale.upgrade() {
                    value_for_event.set_text(&format_for_event(scale.value()));
                    point_value_bubble_at_slider(&scale, &bubble_for_event);
                    bubble_for_event.popup();
                }
            } else {
                bubble_for_event.popdown();
            }
        }
        glib::Propagation::Proceed
    });
    scale.add_controller(events);
    scale.connect_destroy(move |_| bubble.unparent());
}

fn point_value_bubble_at_slider(scale: &gtk::Scale, bubble: &gtk::Popover) {
    let (slider_start, slider_end) = scale.slider_range();
    let range = scale.range_rect();
    let slider_center = slider_start.saturating_add(slider_end.saturating_sub(slider_start) / 2);
    bubble.set_pointing_to(Some(&gtk::gdk::Rectangle::new(
        slider_center,
        range.y(),
        1,
        1,
    )));
}

#[cfg(test)]
mod tests {
    use super::scale_scroll_target;
    #[test]
    fn forwarded_scale_scroll_stays_inside_the_parent_page_range() {
        assert_eq!(
            scale_scroll_target(890.0, 0.0, 1_000.0, 100.0, 10.0, true),
            900.0
        );
        assert_eq!(
            scale_scroll_target(10.0, 0.0, 1_000.0, 100.0, -10.0, true),
            0.0
        );
        let target = scale_scroll_target(200.0, 0.0, 1_000.0, 100.0, 1.0, false);
        assert!((target - 221.544_346_900_318_83).abs() < 1e-9);
    }
}
