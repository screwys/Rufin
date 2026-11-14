use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{CompositeTemplate, TemplateChild, glib};

mod reorder_row_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/preferences/reorder_row.ui")]
    pub(super) struct PreferencesReorderRow {
        #[template_child]
        pub(super) drag: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) up: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) down: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) visible: TemplateChild<gtk::Switch>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesReorderRow {
        const NAME: &'static str = "RufinPreferencesReorderRow";
        type Type = super::PreferencesReorderRow;
        type ParentType = adw::ActionRow;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for PreferencesReorderRow {}
    impl WidgetImpl for PreferencesReorderRow {}
    impl ListBoxRowImpl for PreferencesReorderRow {}
    impl PreferencesRowImpl for PreferencesReorderRow {}
    impl ActionRowImpl for PreferencesReorderRow {}
}

glib::wrapper! {
    struct PreferencesReorderRow(ObjectSubclass<reorder_row_imp::PreferencesReorderRow>)
        @extends gtk::Widget, gtk::ListBoxRow, adw::PreferencesRow, adw::ActionRow,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

/// A row of a reorderable list: drag handle, optional icon, title, move buttons and the
/// switch that shows it.
pub struct ReorderRowPresentation {
    pub title: String,
    pub subtitle: String,
    pub icon: Option<&'static str>,
    pub visible: bool,
    pub visible_sensitive: bool,
    pub up_sensitive: bool,
    pub down_sensitive: bool,
    pub drag_id: String,
}

pub fn reorder_row(
    presentation: ReorderRowPresentation,
    on_visible: impl Fn(bool) + 'static,
    on_move: impl Fn(isize) + 'static,
    on_drop: impl Fn(String, bool) -> bool + 'static,
) -> adw::ActionRow {
    let row: PreferencesReorderRow = glib::Object::new();
    row.set_title(&presentation.title);
    row.set_subtitle(&presentation.subtitle);
    let imp = row.imp();
    if let Some(icon) = presentation.icon {
        imp.icon.set_icon_name(Some(icon));
        imp.icon.set_visible(true);
    }
    imp.visible.set_active(presentation.visible);
    imp.visible.set_sensitive(presentation.visible_sensitive);
    imp.up.set_sensitive(presentation.up_sensitive);
    imp.down.set_sensitive(presentation.down_sensitive);
    row.set_activatable_widget(Some(&imp.visible.get()));

    imp.visible
        .connect_active_notify(move |visible| on_visible(visible.is_active()));
    let on_move = Rc::new(on_move);
    let move_up = Rc::clone(&on_move);
    imp.up.connect_clicked(move |_| move_up(-1));
    imp.down.connect_clicked(move |_| on_move(1));

    let drag_id = presentation.drag_id;
    let source = gtk::DragSource::builder()
        .actions(gtk::gdk::DragAction::MOVE)
        .build();
    source.connect_prepare(move |_, _, _| {
        Some(gtk::gdk::ContentProvider::for_value(&drag_id.to_value()))
    });
    imp.drag.add_controller(source);

    let drop_target = gtk::DropTarget::new(String::static_type(), gtk::gdk::DragAction::MOVE);
    let weak_row = row.downgrade();
    drop_target.connect_drop(move |_, value, _, y| {
        let Ok(source_id) = value.get::<String>() else {
            return false;
        };
        let Some(row) = weak_row.upgrade() else {
            return false;
        };
        on_drop(source_id, y > f64::from(row.height()) / 2.0)
    });
    row.add_controller(drop_target);
    row.upcast()
}

/// Rows for an ordered list of switchable items: the shown ones in their order, then the
/// hidden ones. Covers, artist images and lyrics providers store only the shown ones.
pub struct ProviderList<T> {
    pub shown: Vec<T>,
    pub all: Vec<T>,
}

impl<T: Copy + PartialEq> ProviderList<T> {
    pub fn rows(&self) -> Vec<(T, bool)> {
        self.shown
            .iter()
            .map(|item| (*item, true))
            .chain(
                self.all
                    .iter()
                    .filter(|item| !self.shown.contains(item))
                    .map(|item| (*item, false)),
            )
            .collect()
    }

    /// `item` shown or hidden; a newly shown one goes last of the shown.
    pub fn set_shown(mut self, item: T, shown: bool) -> Vec<T> {
        self.shown.retain(|candidate| *candidate != item);
        if shown {
            self.shown.push(item);
        }
        self.shown
    }

    /// `item` moved by `delta` among the shown ones.
    pub fn moved(mut self, item: T, delta: isize) -> Vec<T> {
        if let Some(index) = self.shown.iter().position(|candidate| *candidate == item) {
            let target = index.saturating_add_signed(delta).min(self.shown.len() - 1);
            let item = self.shown.remove(index);
            self.shown.insert(target, item);
        }
        self.shown
    }

    /// `source` dropped before or after `target`, both shown.
    pub fn dropped(mut self, source: T, target: T, after: bool) -> Vec<T> {
        if source == target || !self.shown.contains(&source) || !self.shown.contains(&target) {
            return self.shown;
        }
        self.shown.retain(|candidate| *candidate != source);
        let index = self
            .shown
            .iter()
            .position(|candidate| *candidate == target)
            .unwrap_or(self.shown.len());
        self.shown.insert(index + usize::from(after), source);
        self.shown
    }
}
