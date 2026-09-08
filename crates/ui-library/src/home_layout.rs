use gtk::subclass::prelude::ObjectSubclassIsExt;

ui_shared::composite_box!(
    pub HomeSectionHeaderView,
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

pub fn home_section_header(title: &str) -> HomeSectionHeaderView {
    let header = HomeSectionHeaderView::new();
    header.imp().heading.set_label(&localization::tr(title));
    header
}

impl HomeSectionHeaderView {
    pub fn set_title(&self, title: &str) {
        self.imp().heading.set_label(title);
    }
    pub fn previous(&self) -> gtk::Button {
        self.imp().previous.get()
    }

    pub fn next(&self) -> gtk::Button {
        self.imp().next.get()
    }

    pub fn refresh(&self) -> gtk::Button {
        self.imp().refresh.get()
    }
}
