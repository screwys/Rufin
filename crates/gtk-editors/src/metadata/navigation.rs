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
                && let Some(tracks) = tracks.upgrade()
                && tracks.is_sensitive()
                && let Some(position) = adjacent(&model, forward)
            {
                model.select_item(position, true);
                tracks.scroll_to(
                    position,
                    gtk::ListScrollFlags::FOCUS | gtk::ListScrollFlags::SELECT,
                    None,
                );
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
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let root_widget = root.as_ref().downgrade();
    keys.connect_key_pressed(move |_, key, _, state| {
        let action = match (key, state & gtk::accelerator_get_default_mod_mask()) {
            (gtk::gdk::Key::Left, gtk::gdk::ModifierType::ALT_MASK) | (gtk::gdk::Key::Back, _) => {
                "tracks.previous"
            }
            (gtk::gdk::Key::Right, gtk::gdk::ModifierType::ALT_MASK)
            | (gtk::gdk::Key::Forward, _) => "tracks.next",
            _ => return gtk::glib::Propagation::Proceed,
        };
        if let Some(root) = root_widget.upgrade() {
            let _ = root.activate_action(action, None);
        }
        gtk::glib::Propagation::Stop
    });
    root.add_controller(keys);
    for (button, action) in [(8, "tracks.previous"), (9, "tracks.next")] {
        let click = gtk::GestureClick::new();
        click.set_button(button);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let root_widget = root.as_ref().downgrade();
        click.connect_pressed(move |gesture, _, _, _| {
            if let Some(root) = root_widget.upgrade() {
                let _ = root.activate_action(action, None);
            }
            gesture.set_state(gtk::EventSequenceState::Claimed);
        });
        root.add_controller(click);
    }
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
