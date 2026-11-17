//! Dragging a row of a list, or a child of a grid, to another place by its handle. It moves among
//! the others while it is dragged, and its new place is committed when it is let go.
use std::cell::RefCell;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;

/// On the widget a row or a grid's child is dragged by.
const HANDLE_CLASS: &str = "reorder-handle";
/// On the row or the grid's child while it is dragged.
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

/// The children of a grid that move, in the order it shows them; see [`install_child_reorder`].
#[derive(Clone)]
pub struct ChildReorder(Rc<ChildDrag>);

#[derive(Default)]
struct ChildDrag {
    order: RefCell<Vec<gtk::Widget>>,
    /// The child dragged, and the order as the drag found it.
    dragged: RefCell<Option<(gtk::Widget, Vec<gtk::Widget>)>>,
}

impl ChildReorder {
    /// The children that move, in the order the grid shows them; it ends a drag.
    pub fn set_order(&self, order: Vec<gtk::Widget>) {
        self.0.dragged.take();
        self.0.order.replace(order);
    }

    /// The place of `child` among the children that move.
    pub fn position(&self, child: &impl IsA<gtk::Widget>) -> Option<usize> {
        let child = child.upcast_ref::<gtk::Widget>();
        self.0
            .order
            .borrow()
            .iter()
            .position(|placed| placed == child)
    }
}

/// The child of `grid` that holds `widget`, and whether a reorder handle is on the way to it.
fn grid_child(grid: &gtk::Widget, mut widget: Option<gtk::Widget>) -> Option<(gtk::Widget, bool)> {
    let mut on_handle = false;
    while let Some(picked) = widget {
        on_handle |= picked.has_css_class(HANDLE_CLASS);
        let parent = picked.parent();
        if parent.as_ref() == Some(grid) {
            return Some((picked, on_handle));
        }
        widget = parent;
    }
    None
}

/// Lets the children of `grid`, such as a `gtk::FlowBox` or an `adw::WrapBox`, that
/// [`ChildReorder::set_order`] lists be dragged by a handle, a descendant with the
/// `reorder-handle` class, to another one's place, as a Radio station's card is. While one is
/// dragged, `show` puts them in the order it is given; when it is let go elsewhere, `dropped`
/// gets it and the new order. The owner keeps the returned order for as long as the grid shows.
pub fn install_child_reorder<W: IsA<gtk::Widget>>(
    grid: &W,
    show: impl Fn(&W, &[gtk::Widget]) + 'static,
    dropped: impl Fn(&gtk::Widget, &[gtk::Widget]) + 'static,
) -> ChildReorder {
    let state = Rc::new(ChildDrag::default());
    // The grid hears the drag, so a child that changes place under the pointer does not end it.
    let drag = gtk::GestureDrag::new();
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&state);
    drag.connect_drag_begin(move |gesture, x, y| {
        let (Some(state), Some(grid)) = (weak.upgrade(), gesture.widget()) else {
            return;
        };
        let Some((child, true)) = grid_child(&grid, grid.pick(x, y, gtk::PickFlags::DEFAULT))
        else {
            return;
        };
        let start = state.order.borrow().clone();
        if !start.contains(&child) {
            return;
        }
        gesture.set_state(gtk::EventSequenceState::Claimed);
        child.add_css_class(DRAGGING_CLASS);
        state.dragged.replace(Some((child, start)));
    });
    let weak = Rc::downgrade(&state);
    drag.connect_drag_update(move |gesture, offset_x, offset_y| {
        let (Some(state), Some(grid), Some((x, y))) = (
            weak.upgrade(),
            gesture.widget().and_downcast::<W>(),
            gesture.start_point(),
        ) else {
            return;
        };
        let Some(dragged) = state
            .dragged
            .borrow()
            .as_ref()
            .map(|(child, _)| child.clone())
        else {
            return;
        };
        let over = grid_child(
            grid.upcast_ref(),
            grid.pick(x + offset_x, y + offset_y, gtk::PickFlags::DEFAULT),
        );
        let Some((over, _)) = over.filter(|(over, _)| *over != dragged) else {
            return;
        };
        let order = {
            let mut order = state.order.borrow_mut();
            let (Some(from), Some(to)) = (
                order.iter().position(|placed| *placed == dragged),
                order.iter().position(|placed| *placed == over),
            ) else {
                return;
            };
            let moved = order.remove(from);
            order.insert(to, moved);
            order.clone()
        };
        show(&grid, &order);
    });
    let weak = Rc::downgrade(&state);
    drag.connect_drag_end(move |_, _, _| {
        let Some(state) = weak.upgrade() else {
            return;
        };
        let Some((dragged, start)) = state.dragged.take() else {
            return;
        };
        dragged.remove_css_class(DRAGGING_CLASS);
        let order = state.order.borrow().clone();
        if order != start {
            dropped(&dragged, &order);
        }
    });
    grid.add_controller(drag);
    ChildReorder(state)
}
