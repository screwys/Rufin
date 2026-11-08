use gtk::prelude::*;
use std::{cell::Cell, rc::Rc};

/// Distance a finger moves before a drag counts as a swipe in one direction.
const SWIPE_DECISION_DISTANCE: f64 = 12.0;
/// Distance a released swipe needs to act.
pub const SWIPE_ACTION_DISTANCE: f64 = 48.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SwipeAxis {
    Horizontal,
    Vertical,
}

/// A touch drag on `widget` that becomes a swipe once it clearly moves along one axis. `claims`
/// decides whether that axis and direction belong to this widget; otherwise the drag is left to the
/// widgets below, so vertical scrolling and taps keep working. `moved` follows the finger and
/// `released` receives the final offsets of a claimed swipe.
pub fn install_touch_swipe(
    widget: &impl IsA<gtk::Widget>,
    claims: impl Fn(SwipeAxis, f64, f64) -> bool + 'static,
    moved: impl Fn(SwipeAxis, f64, f64) + 'static,
    released: impl Fn(SwipeAxis, f64, f64) + 'static,
) {
    let drag = gtk::GestureDrag::new();
    drag.set_touch_only(true);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let axis = Rc::new(Cell::new(None::<SwipeAxis>));
    let begin_axis = Rc::clone(&axis);
    drag.connect_drag_begin(move |_, _, _| begin_axis.set(None));
    let update_axis = Rc::clone(&axis);
    drag.connect_drag_update(move |drag, dx, dy| {
        if let Some(axis) = update_axis.get() {
            moved(axis, dx, dy);
            return;
        }
        if dx.abs().max(dy.abs()) < SWIPE_DECISION_DISTANCE {
            return;
        }
        let axis = if dx.abs() > dy.abs() * 1.5 {
            SwipeAxis::Horizontal
        } else if dy.abs() > dx.abs() * 1.5 {
            SwipeAxis::Vertical
        } else {
            drag.set_state(gtk::EventSequenceState::Denied);
            return;
        };
        if claims(axis, dx, dy) {
            update_axis.set(Some(axis));
            drag.set_state(gtk::EventSequenceState::Claimed);
            moved(axis, dx, dy);
        } else {
            drag.set_state(gtk::EventSequenceState::Denied);
        }
    });
    drag.connect_drag_end(move |_, dx, dy| {
        if let Some(axis) = axis.take() {
            released(axis, dx, dy);
        }
    });
    widget.add_controller(drag);
}
