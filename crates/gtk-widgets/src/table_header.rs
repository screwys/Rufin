use std::{cell::RefCell, rc::Rc};

use gtk::glib;
use gtk::prelude::*;

use rufin_core::settings::layout::LibraryField;

mod header_cell_imp {
    use gtk::prelude::*;
    use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/column_view_header_cell.ui")]
    pub struct HeaderCell {
        #[template_child]
        pub content: TemplateChild<gtk::Box>,
        #[template_child]
        pub label: TemplateChild<gtk::Label>,
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HeaderCell {
        const NAME: &'static str = "RufinColumnViewHeaderCell";
        type Type = super::HeaderCell;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for HeaderCell {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            self.dispose_template();
        }
    }

    impl WidgetImpl for HeaderCell {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Horizontal {
                // The column's fixed width owns horizontal sizing, including clipped labels.
                (0, 0, -1, -1)
            } else {
                self.content.measure(orientation, for_size)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.content.allocate(width, height, baseline, None);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_child(&*self.content, snapshot);
        }
    }
}

glib::wrapper! {
    pub struct HeaderCell(ObjectSubclass<header_cell_imp::HeaderCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl HeaderCell {
    pub fn new(title: &str, field: Option<LibraryField>, position: usize) -> Self {
        use gtk::subclass::prelude::ObjectSubclassIsExt;
        let cell: Self = glib::Object::new();
        let imp = cell.imp();
        cell.update_property(&[gtk::accessible::Property::Label(title)]);
        cell.update_relation(&[gtk::accessible::Relation::ColIndex(position as i32 + 1)]);
        imp.label.set_label(title);
        imp.icon.set_tooltip_text(Some(title));
        imp.label
            .set_xalign(if field == Some(LibraryField::RowIndex) {
                0.5
            } else {
                0.0
            });
        let icon = match field {
            Some(LibraryField::Duration) => Some("rufin-preferences-system-time-symbolic"),
            Some(LibraryField::Favorite) => Some(crate::favorites::FAVORITE_ADD_ICON),
            _ => None,
        };
        imp.label.set_visible(icon.is_none());
        imp.icon
            .set_halign(if field == Some(LibraryField::Favorite) {
                gtk::Align::Center
            } else {
                gtk::Align::Start
            });
        imp.icon.set_icon_name(icon);
        imp.icon.set_visible(icon.is_some());
        cell
    }
}

#[derive(Clone)]
pub struct ColumnViewHeader {
    root: gtk::Box,
    bindings: Rc<RefCell<Vec<glib::Binding>>>,
}

impl Default for ColumnViewHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl ColumnViewHeader {
    pub fn new() -> Self {
        let resource = crate::ui_resource::COLUMN_VIEW_HEADER_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        let root = crate::ui_resource::object(&builder, resource, "header");
        Self {
            root,
            bindings: Rc::default(),
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn replace(&self, columns: &[(gtk::ColumnViewColumn, Option<LibraryField>)]) {
        for binding in self.bindings.take() {
            binding.unbind();
        }
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        let mut bindings = Vec::with_capacity(columns.len() * 2);
        for (position, (column, field)) in columns.iter().enumerate() {
            let title = column.title().unwrap_or_default();
            let cell = HeaderCell::new(&title, *field, position);
            bindings.push(
                column
                    .bind_property("fixed-width", &cell, "width-request")
                    .sync_create()
                    .build(),
            );
            bindings.push(
                column
                    .bind_property("visible", &cell, "visible")
                    .sync_create()
                    .build(),
            );
            self.root.append(&cell);
        }
        self.bindings.replace(bindings);
    }

    pub fn set_visible(&self, visible: bool) {
        self.root.set_visible(visible);
    }
}

#[derive(Clone)]
pub struct TableFrame {
    root: gtk::Box,
    header_host: gtk::Box,
}

impl TableFrame {
    pub fn new(content: &impl IsA<gtk::Widget>, header: Option<&gtk::Widget>) -> Self {
        let resource = crate::ui_resource::TABLE_FRAME_RESOURCE;
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            root: gtk::Box,
            header_host: gtk::Box,
            content_host: gtk::Box,
        });
        content_host.append(content);
        let frame = Self { root, header_host };
        frame.set_header(header);
        frame
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn set_header(&self, header: Option<&gtk::Widget>) {
        while let Some(child) = self.header_host.first_child() {
            self.header_host.remove(&child);
        }
        if let Some(header) = header {
            self.header_host.append(header);
        }
        self.header_host.set_visible(header.is_some());
    }
}
