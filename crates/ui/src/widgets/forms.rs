use adw::prelude::*;
use localization::tr;
use playback::StreamQuality;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
pub fn selection_row<F>(
    title: &str,
    option_titles: &[String],
    selected: u32,
    on_selected: F,
) -> adw::ActionRow
where
    F: Fn(u32) + 'static,
{
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(&selection_buttons(
        option_titles,
        option_titles,
        selected,
        on_selected,
    ));
    row
}

pub fn controlled_selection_row(
    title: &str,
    option_titles: &[String],
    selected: u32,
) -> (adw::ActionRow, Vec<gtk::ToggleButton>) {
    let row = adw::ActionRow::builder().title(title).build();
    let (buttons, controls) = selection_buttons_unconnected(option_titles, option_titles, selected);
    row.add_suffix(&buttons);
    (row, controls)
}

pub fn row_action_content(title: &str, icon_name: &str) -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    content.append(&gtk::Image::from_icon_name(icon_name));
    let label = gtk::Label::new(Some(&tr(title)));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_width_chars(0);
    label.set_max_width_chars(18);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_lines(2);
    content.append(&label);
    content
}

pub fn quality_selection_row<F>(
    title: &str,
    qualities: &[StreamQuality],
    selected: u32,
    on_selected: F,
) -> adw::PreferencesRow
where
    F: Fn(u32) + 'static,
{
    let accessible_titles = qualities
        .iter()
        .copied()
        .map(quality_accessible_title)
        .collect::<Vec<_>>();
    let display_titles = qualities
        .iter()
        .copied()
        .map(quality_button_title)
        .collect::<Vec<_>>();
    let buttons = selection_buttons(&accessible_titles, &display_titles, selected, on_selected);

    let title_group = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    title_group.set_hexpand(true);
    title_group.set_width_request(1);
    title_group.set_valign(gtk::Align::Center);
    let title_label = gtk::Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.set_natural_wrap_mode(gtk::NaturalWrapMode::None);
    title_label.set_wrap(true);
    title_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title_group.append(&title_label);
    let unit = gtk::Label::new(Some(&tr("kbps")));
    unit.add_css_class("dim-label");
    title_group.append(&unit);

    let content = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    content.set_margin_top(8);
    content.set_margin_bottom(8);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&title_group);
    content.append(&buttons);

    adw::PreferencesRow::builder()
        .title(title)
        .child(&content)
        .activatable(false)
        .selectable(false)
        .build()
}

fn selection_buttons<F>(
    accessible_titles: &[String],
    display_titles: &[String],
    selected: u32,
    on_selected: F,
) -> gtk::Box
where
    F: Fn(u32) + 'static,
{
    let (buttons, controls) =
        selection_buttons_unconnected(accessible_titles, display_titles, selected);
    let on_selected = Rc::new(on_selected);
    for (index, button) in controls.iter().enumerate() {
        let on_selected = Rc::clone(&on_selected);
        button.connect_toggled(move |button| {
            if button.is_active() {
                on_selected(index as u32);
            }
        });
    }
    buttons
}

fn selection_buttons_unconnected(
    accessible_titles: &[String],
    display_titles: &[String],
    selected: u32,
) -> (gtk::Box, Vec<gtk::ToggleButton>) {
    debug_assert_eq!(accessible_titles.len(), display_titles.len());
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    buttons.add_css_class("linked");
    buttons.add_css_class("preference-selection-buttons");
    buttons.set_valign(gtk::Align::Center);
    let mut first_button: Option<gtk::ToggleButton> = None;
    let mut controls = Vec::with_capacity(accessible_titles.len());

    for (index, (accessible_title, display_title)) in
        accessible_titles.iter().zip(display_titles).enumerate()
    {
        let button = gtk::ToggleButton::with_label(display_title);
        button.add_css_class("preference-selection-button");
        button.set_tooltip_text(Some(accessible_title));
        button.update_property(&[gtk::accessible::Property::Label(accessible_title)]);
        if let Some(first) = &first_button {
            button.set_group(Some(first));
        } else {
            first_button = Some(button.clone());
        }
        button.set_active(index as u32 == selected);
        buttons.append(&button);
        controls.push(button);
    }

    (buttons, controls)
}

fn quality_accessible_title(quality: StreamQuality) -> String {
    match quality {
        StreamQuality::Original => tr("Original"),
        StreamQuality::MaxBitrateKbps(320) => tr("320 kbps"),
        StreamQuality::MaxBitrateKbps(256) => tr("256 kbps"),
        StreamQuality::MaxBitrateKbps(192) => tr("192 kbps"),
        StreamQuality::MaxBitrateKbps(128) => tr("128 kbps"),
        StreamQuality::MaxBitrateKbps(bitrate) => format!("{bitrate} kbps"),
    }
}

fn quality_button_title(quality: StreamQuality) -> String {
    match quality {
        StreamQuality::Original => tr("Original"),
        StreamQuality::MaxBitrateKbps(bitrate) => bitrate.to_string(),
    }
}

/// Connect the reset icon embedded in an entry.
pub fn connect_entry_reset(entry: &gtk::Entry, reset: impl Fn(&gtk::Entry) + 'static) {
    entry.connect_icon_release(move |entry, position| {
        if position == gtk::EntryIconPosition::Secondary {
            reset(entry);
        }
    });
}

/// Under a server address row, core's `https://` and `http://` completions while the row has
/// focus and the address no scheme; a click fills it in.
pub fn attach_address_suggestions(row: &adw::EntryRow) {
    attach_suggestions(row, |text, show| {
        show(
            rufin_core::source::authorization::address_suggestions(text)
                .into_iter()
                .map(|text| Suggestion { text, detail: None })
                .collect(),
        );
    });
}

/// A suggestion under an entry: the text a pick fills in and, dimmed beside it, a detail.
pub struct Suggestion {
    pub text: String,
    pub detail: Option<String>,
}

/// Under `entry`, the suggestions `suggest` gives for its text while it has focus; a click fills
/// one in. `suggest` hands them over at once or later; those for text that has changed since, or
/// after focus left, are dropped.
pub fn attach_suggestions<E>(
    entry: &E,
    suggest: impl Fn(&str, Box<dyn FnOnce(Vec<Suggestion>)>) + 'static,
) where
    E: IsA<gtk::Widget> + IsA<gtk::Editable>,
{
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("navigation-sidebar");
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .propagate_natural_width(true)
        .max_content_height(320)
        .build();
    let popover = gtk::Popover::builder()
        .child(&scroller)
        .autohide(false)
        .has_arrow(false)
        .position(gtk::PositionType::Bottom)
        .halign(gtk::Align::Start)
        // Typing stays in the entry: neither the popover nor its rows take focus.
        .can_focus(false)
        .build();
    popover.set_parent(entry);
    let texts = Rc::new(RefCell::new(Vec::<String>::new()));
    // Counts requests, so a later one, a pick or the entry leaving makes earlier answers stale.
    let request = Rc::new(Cell::new(0_u64));
    // The entry owns the popover; it leaves with the entry.
    entry.upcast_ref::<gtk::Widget>().connect_destroy({
        let (popover, request) = (popover.clone(), Rc::clone(&request));
        move |_| {
            request.set(request.get() + 1);
            popover.unparent();
        }
    });
    let filling = Rc::new(Cell::new(false));
    let editable = entry.upcast_ref::<gtk::Editable>().downgrade();
    list.connect_row_activated({
        let (texts, request, filling) =
            (Rc::clone(&texts), Rc::clone(&request), Rc::clone(&filling));
        let popover = popover.downgrade();
        move |_, choice| {
            let (Some(editable), Some(popover)) = (editable.upgrade(), popover.upgrade()) else {
                return;
            };
            let Some(text) = texts.borrow().get(choice.index() as usize).cloned() else {
                return;
            };
            request.set(request.get() + 1);
            filling.set(true);
            editable.set_text(&text);
            editable.set_position(-1);
            filling.set(false);
            popover.popdown();
        }
    });
    let focus = gtk::EventControllerFocus::new();
    let show = {
        let focus = focus.downgrade();
        let show: Rc<dyn Fn(&gtk::Editable)> = Rc::new(move |editable: &gtk::Editable| {
            let current = request.get() + 1;
            request.set(current);
            if filling.get() || !focus.upgrade().is_some_and(|focus| focus.contains_focus()) {
                popover.popdown();
                return;
            }
            let (list, popover, texts, request) = (
                list.clone(),
                popover.clone(),
                Rc::clone(&texts),
                Rc::clone(&request),
            );
            suggest(
                &editable.text(),
                Box::new(move |suggestions| {
                    if request.get() != current {
                        return;
                    }
                    list.remove_all();
                    let mut shown = texts.borrow_mut();
                    shown.clear();
                    for suggestion in suggestions {
                        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                        row.append(
                            &gtk::Label::builder()
                                .label(&suggestion.text)
                                .xalign(0.0)
                                .hexpand(true)
                                .ellipsize(gtk::pango::EllipsizeMode::End)
                                .max_width_chars(40)
                                .build(),
                        );
                        if let Some(detail) = &suggestion.detail {
                            let detail = gtk::Label::new(Some(detail));
                            detail.add_css_class("dim-label");
                            detail.add_css_class("numeric");
                            row.append(&detail);
                        }
                        list.append(&row);
                        shown.push(suggestion.text);
                    }
                    if shown.is_empty() {
                        popover.popdown();
                    } else {
                        popover.popup();
                    }
                }),
            );
        });
        show
    };
    entry.upcast_ref::<gtk::Editable>().connect_changed({
        let show = Rc::clone(&show);
        move |editable| show(editable)
    });
    let on_focus = |show: Rc<dyn Fn(&gtk::Editable)>| {
        let editable = entry.upcast_ref::<gtk::Editable>().downgrade();
        move |_: &gtk::EventControllerFocus| {
            if let Some(editable) = editable.upgrade() {
                show(&editable);
            }
        }
    };
    focus.connect_enter(on_focus(show.clone()));
    focus.connect_leave(on_focus(show));
    entry.add_controller(focus);
}
