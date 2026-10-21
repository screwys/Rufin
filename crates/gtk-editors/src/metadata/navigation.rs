use adw::prelude::*;

pub(super) fn connect(
    root: &impl IsA<gtk::Widget>,
    tracks: &gtk::ListView,
    selection: &impl IsA<gtk::SelectionModel>,
) {
    let selection = selection.clone().upcast::<gtk::SelectionModel>();
    let group = gtk::gio::SimpleActionGroup::new();
    let previous = gtk::gio::SimpleAction::new("previous", None);
    let next = gtk::gio::SimpleAction::new("next", None);
    for (action, forward) in [(&previous, false), (&next, true)] {
        let model = selection.downgrade();
        let tracks = tracks.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(model) = model.upgrade()
                && let Some(position) = adjacent(&model, forward)
            {
                model.select_item(position, true);
                if let Some(tracks) = tracks.upgrade() {
                    tracks.scroll_to(position, gtk::ListScrollFlags::NONE, None);
                }
            }
        });
        group.add_action(action);
    }
    previous.set_enabled(adjacent(&selection, false).is_some());
    next.set_enabled(adjacent(&selection, true).is_some());
    let previous = previous.downgrade();
    let next = next.downgrade();
    selection.connect_selection_changed(move |selection, _, _| {
        if let Some(action) = previous.upgrade() {
            action.set_enabled(adjacent(selection, false).is_some());
        }
        if let Some(action) = next.upgrade() {
            action.set_enabled(adjacent(selection, true).is_some());
        }
    });
    root.insert_action_group("tracks", Some(&group));
}

fn adjacent(selection: &gtk::SelectionModel, forward: bool) -> Option<u32> {
    let count = selection.n_items();
    if count == 0 {
        return None;
    }
    let selected = selection.selection();
    if selected.size() != 1 {
        return Some(if forward { 0 } else { count - 1 });
    }
    let index = selected.minimum();
    if forward {
        (index + 1 < count).then_some(index + 1)
    } else {
        index.checked_sub(1)
    }
}
