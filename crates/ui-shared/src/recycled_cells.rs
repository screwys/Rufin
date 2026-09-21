use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};

use crate::artwork::{ArtworkTile, ArtworkTileWeak};
use crate::{downloads::DownloadsState, route::Route};

use crate::detail_links::{DetailLinkBinding, DetailLinks};

pub fn list_cell<T: IsA<gtk::Widget>>(item: &gtk::ListItem) -> Option<T> {
    item.child()?.downcast().ok()
}

pub mod artwork_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/recycled_artwork_cell.ui")]
    pub struct RecycledArtworkCell {
        #[template_child]
        area: TemplateChild<gtk::Overlay>,
        #[template_child]
        image: TemplateChild<gtk::Picture>,
        pub(super) artwork: RefCell<Option<ArtworkTileWeak>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecycledArtworkCell {
        const NAME: &'static str = "RufinRecycledArtworkCell";
        type Type = super::RecycledArtworkCell;
        type ParentType = gtk::Widget;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for RecycledArtworkCell {
        fn constructed(&self) {
            self.parent_constructed();
            self.artwork.replace(Some(
                ArtworkTile::from_widgets(self.area.get(), self.image.get(), 48).downgrade(),
            ));
        }

        fn dispose(&self) {
            self.artwork.take();
            self.dispose_template();
        }
    }

    impl WidgetImpl for RecycledArtworkCell {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.area.request_mode()
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.area.measure(orientation, for_size)
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.area.allocate(width, height, baseline, None);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_child(&*self.area, snapshot);
        }
    }
}

glib::wrapper! {
    pub struct RecycledArtworkCell(ObjectSubclass<artwork_imp::RecycledArtworkCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl RecycledArtworkCell {
    pub fn new(size: i32) -> Self {
        let cell: Self = glib::Object::new();
        cell.artwork().set_square_size(size);
        cell
    }

    pub fn artwork(&self) -> ArtworkTile {
        self.imp()
            .artwork
            .borrow()
            .as_ref()
            .expect("recycled artwork cell is initialized")
            .upgrade()
            .expect("recycled artwork cell widgets are alive")
    }
}

pub mod text_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/recycled_text_cell.ui")]
    pub struct RecycledTextCell {
        #[template_child]
        pub(super) label: TemplateChild<gtk::Label>,
        pub(super) links: RefCell<Option<DetailLinkBinding>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecycledTextCell {
        const NAME: &'static str = "RufinRecycledTextCell";
        type Type = super::RecycledTextCell;
        type ParentType = gtk::Box;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for RecycledTextCell {
        fn dispose(&self) {
            self.links.take();
            self.dispose_template();
        }
    }

    impl WidgetImpl for RecycledTextCell {}
    impl BoxImpl for RecycledTextCell {}
}

glib::wrapper! {
    pub struct RecycledTextCell(ObjectSubclass<text_imp::RecycledTextCell>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl RecycledTextCell {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn label(&self) -> gtk::Label {
        self.imp().label.get()
    }

    pub fn enable_links(&self, navigate: Rc<dyn Fn(Route)>) {
        self.imp()
            .links
            .replace(Some(DetailLinkBinding::new(&self.imp().label, navigate)));
    }

    pub fn bind_links(&self, links: DetailLinks) {
        if let Some(binding) = self.imp().links.borrow().as_ref() {
            binding.bind(links);
        }
    }

    pub fn clear(&self) {
        if let Some(binding) = self.imp().links.borrow().as_ref() {
            binding.clear();
        } else {
            self.imp().label.set_text("");
        }
    }
}

crate::composite_box!(
    pub RecycledBadgedTextCell,
    badged_text_imp,
    "RufinRecycledBadgedTextCell",
    "/io/github/screwys/Rufin/ui/routes/recycled_badged_text_cell.ui",
    {
        label: gtk::Label,
        downloaded: gtk::Image,
    }
);

impl RecycledBadgedTextCell {
    pub fn with_downloads(downloads: &Rc<DownloadsState>) -> Self {
        let cell = Self::new();
        install_playing_indicator(&cell.imp().label, &cell);
        downloads.register_download_badge(&cell.imp().downloaded);
        cell
    }

    pub fn label(&self) -> gtk::Label {
        self.imp().label.get()
    }

    pub fn downloaded(&self) -> gtk::Image {
        self.imp().downloaded.get()
    }

    pub fn clear(&self) {
        self.imp().label.set_text("");
        self.imp().downloaded.set_visible(false);
    }
}

pub mod merged_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/recycled_merged_cell.ui")]
    pub struct RecycledMergedCell {
        #[template_child]
        pub(super) cover: TemplateChild<RecycledArtworkCell>,
        #[template_child]
        pub(super) title_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) title: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) subtitle: TemplateChild<gtk::Label>,
        pub(super) links: RefCell<Option<DetailLinkBinding>>,
        pub(super) downloaded: RefCell<Option<gtk::Image>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecycledMergedCell {
        const NAME: &'static str = "RufinRecycledMergedCell";
        type Type = super::RecycledMergedCell;
        type ParentType = gtk::Box;

        fn class_init(class: &mut Self::Class) {
            RecycledArtworkCell::ensure_type();
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for RecycledMergedCell {
        fn dispose(&self) {
            self.links.take();
            self.downloaded.take();
            self.dispose_template();
        }
    }

    impl WidgetImpl for RecycledMergedCell {}
    impl BoxImpl for RecycledMergedCell {}
}

glib::wrapper! {
    pub struct RecycledMergedCell(ObjectSubclass<merged_imp::RecycledMergedCell>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl RecycledMergedCell {
    pub fn new(
        navigate: Rc<dyn Fn(Route)>,
        downloads: &Rc<DownloadsState>,
        cover_size: i32,
        downloaded: bool,
    ) -> Self {
        let cell: Self = glib::Object::new();
        let imp = cell.imp();
        install_playing_indicator(&imp.title, &*imp.title_row);
        imp.cover.artwork().set_square_size(cover_size);
        imp.links
            .replace(Some(DetailLinkBinding::new(&imp.subtitle, navigate)));
        if downloaded {
            let badge = downloads.download_badge(false);
            imp.title_row.append(&badge);
            imp.downloaded.replace(Some(badge));
        }
        cell
    }

    pub fn without_downloads(navigate: Rc<dyn Fn(Route)>, cover_size: i32) -> Self {
        let cell: Self = glib::Object::new();
        let imp = cell.imp();
        install_playing_indicator(&imp.title, &*imp.title_row);
        imp.cover.artwork().set_square_size(cover_size);
        imp.links
            .replace(Some(DetailLinkBinding::new(&imp.subtitle, navigate)));
        cell
    }

    pub fn cover(&self) -> ArtworkTile {
        self.imp().cover.artwork()
    }

    pub fn title(&self) -> gtk::Label {
        self.imp().title.get()
    }

    pub fn subtitle(&self) -> gtk::Label {
        self.imp().subtitle.get()
    }

    pub fn downloaded(&self) -> Option<gtk::Image> {
        self.imp().downloaded.borrow().clone()
    }

    pub fn bind_subtitle(&self, links: DetailLinks) {
        if let Some(binding) = self.imp().links.borrow().as_ref() {
            binding.bind(links);
        }
    }

    pub fn clear_subtitle(&self) {
        if let Some(binding) = self.imp().links.borrow().as_ref() {
            binding.clear();
        }
        self.imp().subtitle.set_visible(false);
    }
}

pub mod folder_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/recycled_folder_cell.ui")]
    pub struct RecycledFolderCell {
        #[template_child]
        pub(super) icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) cover: TemplateChild<RecycledArtworkCell>,
        #[template_child]
        pub(super) label: TemplateChild<gtk::Label>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecycledFolderCell {
        const NAME: &'static str = "RufinRecycledFolderCell";
        type Type = super::RecycledFolderCell;
        type ParentType = gtk::Box;

        fn class_init(class: &mut Self::Class) {
            RecycledArtworkCell::ensure_type();
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for RecycledFolderCell {
        fn dispose(&self) {
            self.dispose_template();
        }
    }

    impl WidgetImpl for RecycledFolderCell {}
    impl BoxImpl for RecycledFolderCell {}
}

glib::wrapper! {
    pub struct RecycledFolderCell(ObjectSubclass<folder_imp::RecycledFolderCell>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl RecycledFolderCell {
    pub fn new() -> Self {
        let cell: Self = glib::Object::new();
        cell.imp().cover.artwork().set_square_size(48);
        cell
    }

    pub fn icon(&self) -> gtk::Image {
        self.imp().icon.get()
    }

    pub fn cover(&self) -> ArtworkTile {
        self.imp().cover.artwork()
    }

    pub fn set_cover_visible(&self, visible: bool) {
        self.imp().cover.set_visible(visible);
    }

    pub fn label(&self) -> gtk::Label {
        self.imp().label.get()
    }
}

pub fn track_row_index_cell(text: &str, activate: impl Fn(&gtk::Button) + 'static) -> gtk::Overlay {
    let resource = crate::ui_resource::TRACK_ROW_INDEX_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, { cell: gtk::Overlay, number: gtk::Label, play: gtk::Button });
    let click_cell = cell.downgrade();
    play.connect_clicked(move |button| {
        if click_cell
            .upgrade()
            .is_some_and(|cell| cell.has_css_class("track-row-playing"))
        {
            let _ = button.activate_action("win.play-pause", None);
        } else {
            activate(button);
        }
    });
    let number_play = play.downgrade();
    number.connect_label_notify(move |number| {
        if let Some(play) = number_play.upgrade() {
            play.set_visible(!number.text().is_empty());
        }
    });
    let weak_play = play.downgrade();
    cell.connect_css_classes_notify(move |cell| {
        let Some(play) = weak_play.upgrade() else {
            return;
        };
        let playing =
            cell.has_css_class("track-row-playing") && !cell.has_css_class("track-row-paused");
        play.set_icon_name(if playing {
            "rufin-media-playback-pause-symbolic"
        } else {
            "rufin-media-playback-start-symbolic"
        });
        crate::localization::bind_widget_tooltip(
            &play,
            if playing { "Pause" } else { "Play track" },
        );
    });
    number.set_text(text);
    play.set_visible(!text.is_empty());
    cell
}

pub fn track_list_row_index_cell(item: &gtk::ListItem) -> gtk::Overlay {
    let item = item.downgrade();
    track_row_index_cell("", move |button| {
        if let Some(item) = item.upgrade()
            && item.is_activatable()
        {
            let _ =
                button.activate_action("list.activate-item", Some(&item.position().to_variant()));
        }
    })
}

pub fn set_track_playing(title: &gtk::Widget, playing: bool, paused: bool) {
    let mut classes = [
        ("track-row-playing", playing),
        ("track-row-paused", playing && paused),
    ];
    // Set paused first so a newly bound paused title never starts an indicator.
    if playing && paused {
        classes.swap(0, 1);
    }
    for (class, active) in classes {
        if active {
            title.add_css_class(class);
        } else {
            title.remove_css_class(class);
        }
    }
}

pub fn install_playing_indicator(title: &gtk::Label, title_row: &impl IsA<gtk::Box>) {
    let title_row = title_row.as_ref().downgrade();
    let indicator = RefCell::new(None::<crate::playing_indicator::PlayingIndicator>);
    title.connect_css_classes_notify(move |title| {
        let Some(title_row) = title_row.upgrade() else {
            return;
        };
        let playing =
            title.has_css_class("track-row-playing") && !title.has_css_class("track-row-paused");
        if playing && indicator.borrow().is_none() {
            let widget = crate::playing_indicator::PlayingIndicator::new();
            title_row.prepend(&widget);
            indicator.replace(Some(widget));
        } else if !playing && let Some(widget) = indicator.take() {
            title_row.remove(&widget);
        }
    });
}

pub fn set_track_row_index_text(cell: &gtk::Overlay, text: &str) {
    let Some(label) = cell
        .child()
        .and_then(|child| child.downcast::<gtk::Label>().ok())
    else {
        return;
    };
    label.set_text(text);
}
