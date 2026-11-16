//! Dragging a row of a list to another place by its handle. The row moves among the others while
//! it is dragged, as a Radio station's card does, and its new place is committed when it is let
//! go.
use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// On the widget a row is dragged by.
const HANDLE_CLASS: &str = "reorder-handle";
/// On the row while it is dragged.
const DRAGGING_CLASS: &str = "reorder-dragging";

type Dropped = Rc<dyn Fn(String, bool) -> bool>;

struct Entry {
    row: glib::WeakRef<gtk::ListBoxRow>,
    handle: glib::WeakRef<gtk::Widget>,
    id: String,
    dropped: Dropped,
}

struct Drag {
    row: gtk::ListBoxRow,
    /// The rows as the drag found them, and as it has moved them.
    start: Vec<gtk::ListBoxRow>,
    order: Vec<gtk::ListBoxRow>,
}

#[derive(Default)]
struct List {
    entries: RefCell<Vec<Entry>>,
    drag: RefCell<Option<Drag>>,
}

impl List {
    /// The entry of `row` while its handle is sensitive: a row that can be dragged, and moved
    /// aside for another.
    fn movable(&self, row: &gtk::ListBoxRow) -> Option<(String, Dropped)> {
        self.entries
            .borrow()
            .iter()
            .find(|entry| entry.row.upgrade().as_ref() == Some(row))
            .filter(|entry| {
                entry
                    .handle
                    .upgrade()
                    .is_some_and(|handle| handle.is_sensitive())
            })
            .map(|entry| (entry.id.clone(), Rc::clone(&entry.dropped)))
    }
}

thread_local! {
    /// The lists whose rows are dragged, each with the gesture's state.
    static LISTS: RefCell<Vec<(glib::WeakRef<gtk::ListBox>, Rc<List>)>> =
        const { RefCell::new(Vec::new()) };
}

/// Shows `list`'s rows in `order`; rows it leaves out go last.
pub fn order_rows(list: &gtk::ListBox, order: &[gtk::ListBoxRow]) {
    let order = order.to_vec();
    list.set_sort_func(move |a, b| {
        let place = |row: &gtk::ListBoxRow| {
            order
                .iter()
                .position(|placed| placed == row)
                .unwrap_or(usize::MAX)
        };
        place(a).cmp(&place(b)).into()
    });
    list.invalidate_sort();
    // The rows stay where the sort put them, and rows added later go where they are added.
    list.unset_sort_func();
}

fn rows(list: &gtk::ListBox) -> Vec<gtk::ListBoxRow> {
    let mut rows = Vec::new();
    let mut child = list.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if let Ok(row) = widget.downcast::<gtk::ListBoxRow>() {
            rows.push(row);
        }
    }
    rows
}

/// Lets `row` be dragged by `handle` to another place among the rows of its list that have a
/// handle of their own. A row whose handle is insensitive stays where it is. When the row is
/// let go, `dropped` of the row it lands beside is called with the dragged row's `id` and
/// whether it landed after that row; false puts the rows back.
pub fn install_row_reorder(
    row: &impl IsA<gtk::ListBoxRow>,
    handle: &impl IsA<gtk::Widget>,
    id: impl Into<String>,
    dropped: impl Fn(String, bool) -> bool + 'static,
) {
    let handle = handle.upcast_ref::<gtk::Widget>();
    handle.add_css_class(HANDLE_CLASS);
    handle.set_cursor_from_name(Some("grab"));
    let handle = handle.downgrade();
    let id = id.into();
    let dropped: Dropped = Rc::new(dropped);
    let enter = move |row: &gtk::ListBoxRow| {
        let Some(list) = row.parent().and_downcast::<gtk::ListBox>() else {
            return;
        };
        let state = state(&list);
        let mut entries = state.entries.borrow_mut();
        entries.retain(|entry| {
            entry
                .row
                .upgrade()
                .is_some_and(|placed| placed != *row && placed.parent().is_some())
        });
        entries.push(Entry {
            row: row.downgrade(),
            handle: handle.clone(),
            id: id.clone(),
            dropped: Rc::clone(&dropped),
        });
    };
    let row = row.upcast_ref::<gtk::ListBoxRow>();
    enter(row);
    row.connect_parent_notify(enter);
}

/// The drag state of `list`, whose gesture is installed with its first row.
fn state(list: &gtk::ListBox) -> Rc<List> {
    LISTS.with_borrow_mut(|lists| {
        lists.retain(|(placed, _)| placed.upgrade().is_some());
        if let Some((_, state)) = lists
            .iter()
            .find(|(placed, _)| placed.upgrade().as_ref() == Some(list))
        {
            return Rc::clone(state);
        }
        let state = Rc::new(List::default());
        install_gesture(list, &state);
        lists.push((list.downgrade(), Rc::clone(&state)));
        state
    })
}

/// The list hears the drag, so a row that changes place under the pointer does not end it.
fn install_gesture(list: &gtk::ListBox, state: &Rc<List>) {
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(state);
    drag.connect_drag_begin(move |gesture, x, y| {
        let (Some(state), Some(list)) = (
            weak.upgrade(),
            gesture.widget().and_downcast::<gtk::ListBox>(),
        ) else {
            return;
        };
        let mut picked = list.pick(x, y, gtk::PickFlags::DEFAULT);
        let mut on_handle = false;
        let row = loop {
            let Some(widget) = picked else {
                break None;
            };
            on_handle |= widget.has_css_class(HANDLE_CLASS);
            if let Some(row) = widget.downcast_ref::<gtk::ListBoxRow>()
                && row.parent().as_ref() == Some(list.upcast_ref())
            {
                break Some(row.clone());
            }
            picked = widget.parent();
        };
        let Some(row) = row.filter(|row| on_handle && state.movable(row).is_some()) else {
            return;
        };
        gesture.set_state(gtk::EventSequenceState::Claimed);
        row.add_css_class(DRAGGING_CLASS);
        let start = rows(&list);
        state.drag.replace(Some(Drag {
            row,
            order: start.clone(),
            start,
        }));
    });
    let weak = Rc::downgrade(state);
    drag.connect_drag_update(move |gesture, _, offset_y| {
        let (Some(state), Some(list), Some((_, start_y))) = (
            weak.upgrade(),
            gesture.widget().and_downcast::<gtk::ListBox>(),
            gesture.start_point(),
        ) else {
            return;
        };
        let y = start_y + offset_y;
        let order = {
            let mut drag = state.drag.borrow_mut();
            let Some(drag) = drag.as_mut() else {
                return;
            };
            let Some(over) = list
                .row_at_y(y as i32)
                .filter(|over| *over != drag.row && state.movable(over).is_some())
            else {
                return;
            };
            let (Some(from), Some(to), Some(bounds)) = (
                drag.order.iter().position(|placed| *placed == drag.row),
                drag.order.iter().position(|placed| *placed == over),
                over.compute_bounds(&list),
            ) else {
                return;
            };
            // The row takes the other's place once the pointer is past its middle, so rows of
            // different heights do not trade places back and forth.
            let middle = f64::from(bounds.y() + bounds.height() / 2.0);
            if (to > from) != (y > middle) {
                return;
            }
            let moved = drag.order.remove(from);
            drag.order.insert(to, moved);
            drag.order.clone()
        };
        order_rows(&list, &order);
    });
    let weak = Rc::downgrade(state);
    drag.connect_drag_end(move |gesture, _, _| {
        let (Some(state), Some(list)) = (
            weak.upgrade(),
            gesture.widget().and_downcast::<gtk::ListBox>(),
        ) else {
            return;
        };
        let Some(drag) = state.drag.take() else {
            return;
        };
        drag.row.remove_css_class(DRAGGING_CLASS);
        if drag.order == drag.start {
            return;
        }
        let Some((id, _)) = state.movable(&drag.row) else {
            return;
        };
        // The dragged row lands before the row that follows it, or after the one it follows.
        let place = drag.order.iter().position(|placed| *placed == drag.row);
        let beside = |offset: isize, after: bool| {
            let row = drag.order.get(place?.checked_add_signed(offset)?)?;
            Some((state.movable(row)?.1, after))
        };
        let landed = beside(1, false)
            .or_else(|| beside(-1, true))
            .is_some_and(|(dropped, after)| dropped(id, after));
        if !landed {
            order_rows(&list, &drag.start);
        }
    });
    list.add_controller(drag);
}
