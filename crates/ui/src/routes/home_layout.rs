use gtk::subclass::prelude::ObjectSubclassIsExt;

crate::ui_resource::composite_box!(
    pub(crate) HomeSectionHeaderView,
    home_section_header_view_imp,
    "RufinHomeSectionHeaderView",
    "/io/github/screwys/Rufin/ui/routes/home_section_header.ui",
    {
        heading: gtk::Label,
        previous: gtk::Button,
        next: gtk::Button,
        refresh: gtk::Button,
    }
);

pub(super) fn home_section_header(title: &str) -> HomeSectionHeaderView {
    let header = HomeSectionHeaderView::new();
    header.imp().heading.set_label(&localization::tr(title));
    header
}

impl HomeSectionHeaderView {
    pub(super) fn previous(&self) -> gtk::Button {
        self.imp().previous.get()
    }

    pub(super) fn next(&self) -> gtk::Button {
        self.imp().next.get()
    }

    pub(super) fn refresh(&self) -> gtk::Button {
        self.imp().refresh.get()
    }
}
