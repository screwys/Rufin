use gtk::prelude::*;
use localization::tr;

pub(crate) fn localized_label(message: &str) -> gtk::Label {
    gtk::Label::new(Some(&tr(message)))
}

pub(crate) fn bind_label_text_with(label: &gtk::Label, text: impl Fn() -> String + 'static) {
    label.set_text(&text());
}

pub(crate) fn bind_widget_tooltip(widget: &impl IsA<gtk::Widget>, message: &str) {
    widget.set_tooltip_text(Some(&tr(message)));
}

pub(crate) fn bind_widget_accessible_label(
    widget: &(impl IsA<gtk::Widget> + IsA<gtk::Accessible>),
    message: &str,
) {
    let label = tr(message);
    widget.update_property(&[gtk::accessible::Property::Label(&label)]);
}

pub(crate) fn bind_widget_tooltip_with(
    widget: &impl IsA<gtk::Widget>,
    text: impl Fn() -> String + 'static,
) {
    let text = text();
    widget.set_tooltip_text((!text.is_empty()).then_some(text.as_str()));
}

pub(crate) fn bind_search_placeholder(entry: &gtk::SearchEntry, message: &str) {
    entry.set_placeholder_text(Some(&tr(message)));
}

pub(crate) fn bind_column_title(column: &gtk::ColumnViewColumn, message: &str) {
    column.set_title(Some(&tr(message)));
}

pub(crate) fn localized_column(
    message: &str,
    factory: &impl IsA<gtk::ListItemFactory>,
) -> gtk::ColumnViewColumn {
    let column = gtk::ColumnViewColumn::new(None, Some(factory.as_ref().clone()));
    bind_column_title(&column, message);
    column
}

pub(crate) fn bind_drop_down_options_with(
    drop_down: &gtk::DropDown,
    messages: impl Fn() -> Vec<&'static str> + 'static,
    width: impl Fn(&[String]) -> i32 + 'static,
) {
    let translated = messages()
        .iter()
        .map(|message| tr(message))
        .collect::<Vec<_>>();
    let Some(model) = drop_down
        .model()
        .and_then(|model| model.downcast::<gtk::StringList>().ok())
    else {
        return;
    };
    let translated_refs = translated.iter().map(String::as_str).collect::<Vec<_>>();
    model.splice(0, model.n_items(), &translated_refs);
    drop_down.set_width_request(width(&translated));
}
