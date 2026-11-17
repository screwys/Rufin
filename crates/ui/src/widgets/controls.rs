use adw::prelude::*;
use localization::tr;

pub const PLAY_ICON: &str = "rufin-media-playback-start-symbolic";
pub const PLAY_NEXT_ICON: &str = "rufin-mail-forward-symbolic";
pub const PLAY_LATER_ICON: &str = "rufin-go-last-symbolic";
pub const EDIT_ICON: &str = "rufin-document-edit-symbolic";
pub const ADD_ICON: &str = "rufin-list-add-symbolic";
pub const REMOVE_ICON: &str = "rufin-list-remove-symbolic";
pub const DELETE_ICON: &str = "rufin-process-stop-symbolic";
pub const TRASH_ICON: &str = "rufin-user-trash-symbolic";
const SORT_ORDER_ICON: &str = "rufin-sort-name-symbolic";
const SORT_ORDER_DESCENDING_ICON: &str = "rufin-sort-name-descending-symbolic";

pub fn configure_dialog_header(header: &adw::HeaderBar) {
    if let Some(settings) = gtk::Settings::default() {
        settings
            .bind_property("gtk-decoration-layout", header, "decoration-layout")
            .sync_create()
            .build();
    }
}

pub fn configure_search_entry(entry: &gtk::SearchEntry) {
    entry
        .first_child()
        .and_downcast::<gtk::Image>()
        .expect("search entry icon")
        .set_icon_name(Some("rufin-search-symbolic"));
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let search = entry.downgrade();
    click.connect_pressed(move |_, _, _, _| {
        if let Some(search) = search.upgrade() {
            search.grab_focus();
        }
    });
    entry.add_controller(click);
}

pub fn sort_order_icon(descending: bool) -> &'static str {
    if descending {
        SORT_ORDER_DESCENDING_ICON
    } else {
        SORT_ORDER_ICON
    }
}

pub fn set_active_class(widget: &impl IsA<gtk::Widget>, active: bool) {
    if active {
        widget.add_css_class("active-toggle");
    } else {
        widget.remove_css_class("active-toggle");
    }
}
/// A row's play controls at its end, shown while the pointer is over the row or its menu holds
/// it: the grid cover's Play group, Play in the accent with Play Next and Play Later coming out
/// of its sides. The hover controls of the sidebar's Saved rows and Home's rows.
pub fn install_row_transport_controls(
    row: &gtk::Overlay,
    play: impl Fn(rufin_core::playback::QueuePlacement) + 'static,
) {
    use crate::widgets::interactions::CONTEXT_MENU_HOVER_HELD_CLASS;
    use rufin_core::playback::QueuePlacement;
    let play = std::rc::Rc::new(play);
    let (morph, buttons) = crate::widgets::cover_controls::play_group("Play");
    // The block hides the row's text under it; the Play group opens from its centre.
    morph.set_hexpand(true);
    let group = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    group.add_css_class("row-play-group");
    group.set_halign(gtk::Align::End);
    group.set_valign(gtk::Align::Center);
    group.append(&morph);
    for (button, placement) in buttons.iter().zip([
        QueuePlacement::Now,
        QueuePlacement::Next,
        QueuePlacement::Last,
    ]) {
        let play = std::rc::Rc::clone(&play);
        button.connect_clicked(move |_| play(placement));
    }
    row.add_overlay(&group);
    row.set_measure_overlay(&group, false);

    let controls: std::rc::Rc<[gtk::Widget]> = std::rc::Rc::new([group.upcast()]);
    let show = move |controls: &[gtk::Widget], shown: bool| {
        for control in controls {
            control.set_visible(shown);
        }
    };
    show(&controls, false);
    let motion = gtk::EventControllerMotion::new();
    let entered = std::rc::Rc::clone(&controls);
    motion.connect_enter(move |_, _, _| show(&entered, true));
    let left = std::rc::Rc::clone(&controls);
    let row_for_leave = row.downgrade();
    motion.connect_leave(move |_| {
        if row_for_leave
            .upgrade()
            .is_some_and(|row| !row.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS))
        {
            show(&left, false);
        }
    });
    // A context menu holds the controls open; when it lets go, they follow the pointer.
    let pointer = motion.clone();
    let was_held = std::cell::Cell::new(false);
    row.connect_css_classes_notify(move |row| {
        let held = row.has_css_class(CONTEXT_MENU_HOVER_HELD_CLASS);
        if was_held.replace(held) && !held {
            show(&controls, pointer.contains_pointer());
        }
    });
    row.add_controller(motion);
}

pub fn text_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("pill-button");
    button.add_css_class("pill");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.append(&gtk::Image::from_icon_name(icon_name));
    content.append(&gtk::Label::new(Some(&tr(label))));
    button.set_child(Some(&content));
    button
}
