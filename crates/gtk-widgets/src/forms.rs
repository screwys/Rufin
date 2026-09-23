use adw::prelude::*;
use localization::tr;
use playback::StreamQuality;
use std::rc::Rc;
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
