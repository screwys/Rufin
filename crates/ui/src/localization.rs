use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
};

use gtk::glib;
use gtk::prelude::*;
use localization::tr;

thread_local! {
    static OBJECT_LOCALE_BINDINGS: RefCell<BTreeMap<u64, ObjectLocaleBinding>> =
        const { RefCell::new(BTreeMap::new()) };
    static NEXT_OBJECT_LOCALE_BINDING_ID: Cell<u64> = const { Cell::new(0) };
}

struct ObjectLocaleBinding {
    object: glib::WeakRef<glib::Object>,
    apply: Rc<dyn Fn(&glib::Object)>,
}

pub(crate) fn localized_label(message: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    bind_label_text(&label, message);
    label
}

pub(crate) fn bind_label_text(label: &gtk::Label, message: &str) {
    let message = message.to_string();
    bind_object_locale(label, move |object| {
        object
            .downcast_ref::<gtk::Label>()
            .expect("label locale binding kept on a label")
            .set_text(&tr(&message));
    });
}

pub(crate) fn bind_label_text_with(label: &gtk::Label, text: impl Fn() -> String + 'static) {
    bind_object_locale(label, move |object| {
        object
            .downcast_ref::<gtk::Label>()
            .expect("label locale binding kept on a label")
            .set_text(&text());
    });
}

pub(crate) fn bind_widget_tooltip(widget: &impl IsA<gtk::Widget>, message: &str) {
    let message = message.to_string();
    bind_widget_tooltip_with(widget, move || tr(&message));
}

pub(crate) fn bind_widget_accessible_label(widget: &impl IsA<gtk::Widget>, message: &str) {
    let message = message.to_string();
    bind_object_locale(widget.as_ref(), move |object| {
        let widget = object
            .downcast_ref::<gtk::Widget>()
            .expect("accessible-label locale binding kept on a widget");
        let label = tr(&message);
        widget.update_property(&[gtk::accessible::Property::Label(&label)]);
    });
}

pub(crate) fn bind_widget_tooltip_with(
    widget: &impl IsA<gtk::Widget>,
    text: impl Fn() -> String + 'static,
) {
    bind_object_locale(widget.as_ref(), move |object| {
        let widget = object
            .downcast_ref::<gtk::Widget>()
            .expect("tooltip locale binding kept on a widget");
        let text = text();
        widget.set_tooltip_text((!text.is_empty()).then_some(text.as_str()));
    });
}

pub(crate) fn bind_search_placeholder(entry: &gtk::SearchEntry, message: &str) {
    let message = message.to_string();
    bind_object_locale(entry, move |object| {
        object
            .downcast_ref::<gtk::SearchEntry>()
            .expect("search locale binding kept on a search entry")
            .set_placeholder_text(Some(&tr(&message)));
    });
}

pub(crate) fn bind_column_title(column: &gtk::ColumnViewColumn, message: &str) {
    let message = message.to_string();
    bind_object_locale(column, move |object| {
        object
            .downcast_ref::<gtk::ColumnViewColumn>()
            .expect("column locale binding kept on a column")
            .set_title(Some(&tr(&message)));
    });
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
    bind_object_locale(drop_down, move |object| {
        let drop_down = object
            .downcast_ref::<gtk::DropDown>()
            .expect("drop-down locale binding kept on a drop-down");
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
    });
}

pub(crate) fn relocalize_bound_widgets() {
    OBJECT_LOCALE_BINDINGS.with(|bindings| {
        let current = bindings.take();
        for (id, binding) in current {
            let Some(object) = binding.object.upgrade() else {
                continue;
            };
            (binding.apply)(&object);
            let still_alive = binding.object.upgrade();
            drop(object);
            let Some(object) = still_alive else {
                continue;
            };
            bindings.borrow_mut().insert(id, binding);
            drop(object);
        }
    });
}

fn bind_object_locale(object: &impl IsA<glib::Object>, binding: impl Fn(&glib::Object) + 'static) {
    let object = object.as_ref();
    let id = NEXT_OBJECT_LOCALE_BINDING_ID.with(|next| {
        let id = next.get();
        next.set(id.wrapping_add(1));
        id
    });
    let apply = Rc::new(binding);
    apply(object);
    let weak = glib::WeakRef::new();
    weak.set(Some(object));
    OBJECT_LOCALE_BINDINGS.with(|bindings| {
        bindings.borrow_mut().insert(
            id,
            ObjectLocaleBinding {
                object: weak,
                apply,
            },
        );
    });
    let _ = object.add_weak_ref_notify_local(move || {
        let _ = OBJECT_LOCALE_BINDINGS.try_with(|bindings| {
            if let Ok(mut bindings) = bindings.try_borrow_mut() {
                bindings.remove(&id);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DropProbe(Rc<Cell<bool>>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.set(true);
        }
    }

    #[test]
    fn live_locale_binding_relocalizes_and_releases_payload_on_object_teardown() {
        let payload_dropped = Rc::new(Cell::new(false));
        let updates = Rc::new(Cell::new(0));
        let object = glib::Object::new::<glib::Object>();
        let probe = DropProbe(Rc::clone(&payload_dropped));
        let binding_updates = Rc::clone(&updates);
        bind_object_locale(&object, move |_| {
            let _ = &probe;
            binding_updates.set(binding_updates.get() + 1);
        });

        assert!(!payload_dropped.get());
        assert_eq!(updates.get(), 1);
        relocalize_bound_widgets();
        assert_eq!(updates.get(), 2);

        drop(object);

        assert!(payload_dropped.get());
        relocalize_bound_widgets();
        assert_eq!(updates.get(), 2);
    }
}
