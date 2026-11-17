use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

use ::library::{AlbumRow, AlbumView, ArtistView, PlaylistListRow, Row};
use adw::prelude::*;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};
use localization::{msgid, tr};
use rufin_core::artwork::ArtworkBinding;
use rufin_core::playback::{QueuePlacement, RadioPlayRequest};
use tracing::warn;

use crate::favorites::{favorite_button_is_active, set_favorite_button_active};
use crate::library::CatalogUi;
use crate::library::route_layout::{
    DETAIL_SHOWCASE_NARROW_WIDTH as NARROW_WIDTH, PRIMARY_ROUTE_MARGIN_END,
    PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, detail_showcase_cover_size,
};
use crate::menus::media_menus::{CollectionPlay, row_favorite};
use crate::widgets::artwork::{ArtworkTile, CoverGroupProjection};
use crate::widgets::interactions::{ContextMenuOpen, install_context_menu_openers};
use crate::widgets::layout::{AllocationOwner, width_allocation_owner};
use crate::widgets::localization::bind_label_text_with;
use crate::widgets::media_drag::MediaDragSource;
use rufin_core::route::Route;

/// From this width titles take their largest size.
const LARGE_WIDTH: i32 = 840;
const STEP_BUTTON_SIZE: i32 = 34;

mod showcase_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/detail_showcase.ui")]
    pub struct DetailShowcaseView {
        #[template_child]
        pub layers: TemplateChild<gtk::Overlay>,
        #[template_child]
        pub band: TemplateChild<gtk::DrawingArea>,
        #[template_child]
        pub body: TemplateChild<gtk::Box>,
        #[template_child]
        pub cover_host: TemplateChild<gtk::Box>,
        #[template_child]
        pub heading: TemplateChild<gtk::Box>,
        #[template_child]
        pub names: TemplateChild<gtk::Box>,
        #[template_child]
        pub title_slide: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub title_viewport: TemplateChild<gtk::Viewport>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub detail_fade: TemplateChild<crate::widgets::edge_fade::TextFade>,
        #[template_child]
        pub detail_slide: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub detail_viewport: TemplateChild<gtk::Viewport>,
        #[template_child]
        pub detail: TemplateChild<gtk::Label>,
        #[template_child]
        pub meta: TemplateChild<AllocationOwner>,
        #[template_child]
        pub meta_line: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub genre_more: TemplateChild<gtk::Button>,
        #[template_child]
        pub facts_fade: TemplateChild<crate::widgets::edge_fade::TextFade>,
        #[template_child]
        pub facts_slide: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub facts_viewport: TemplateChild<gtk::Viewport>,
        #[template_child]
        pub summary: TemplateChild<gtk::Box>,
        #[template_child]
        pub summary_item_0: TemplateChild<gtk::Box>,
        #[template_child]
        pub summary_icon_0: TemplateChild<gtk::Image>,
        #[template_child]
        pub summary_label_0: TemplateChild<gtk::Label>,
        #[template_child]
        pub summary_item_1: TemplateChild<gtk::Box>,
        #[template_child]
        pub summary_icon_1: TemplateChild<gtk::Image>,
        #[template_child]
        pub summary_label_1: TemplateChild<gtk::Label>,
        #[template_child]
        pub summary_item_2: TemplateChild<gtk::Box>,
        #[template_child]
        pub summary_icon_2: TemplateChild<gtk::Image>,
        #[template_child]
        pub summary_label_2: TemplateChild<gtk::Label>,
        #[template_child]
        pub external_links: TemplateChild<gtk::Box>,
        #[template_child]
        pub actions: TemplateChild<gtk::Box>,
        #[template_child]
        pub play: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub radio: TemplateChild<gtk::Button>,
        #[template_child]
        pub radio_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub favorite: TemplateChild<gtk::Button>,
        #[template_child]
        pub edit: TemplateChild<gtk::Button>,
        #[template_child]
        pub download: TemplateChild<gtk::Button>,
        #[template_child]
        pub more: TemplateChild<gtk::Button>,
        #[template_child]
        pub step_previous: TemplateChild<gtk::Button>,
        #[template_child]
        pub step_next: TemplateChild<gtk::Button>,
        /// The row the header shows, which its buttons act on.
        pub row: RefCell<Option<Row>>,
        pub(crate) shell: RefCell<Weak<CatalogUi>>,
        pub offers_download: Cell<bool>,
        /// The cover's Artwork button, under the pointer while the row's picture can change.
        pub artwork_button: RefCell<Option<gtk::Button>>,
        pub artwork_writable: Cell<bool>,
        /// Play's and Radio's widths with their labels, to tell whether the labels fit again.
        pub labelled_play: Cell<i32>,
        pub labelled_radio: Cell<i32>,
        /// The band's gradient stops, from the cover once it shows.
        pub band_stops: Cell<[[u8; 3]; 3]>,
        /// The shape and width last laid out, to fit the text again when it changes.
        pub fitted: Cell<Option<(ShowcaseShape, i32)>>,
        /// Whether the line starts with facts: a summary's, or an artist's counts.
        pub has_facts: Cell<bool>,
        /// The facts and the genres as one line.
        pub(crate) meta_fit: std::cell::OnceCell<Rc<MetaLine>>,
        /// The header as a strip, on a page that scrolls the header away.
        pub(crate) strip: RefCell<Option<DetailStrip>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DetailShowcaseView {
        const NAME: &'static str = "RufinDetailShowcaseView";
        type Type = super::DetailShowcaseView;
        type ParentType = gtk::Box;

        fn class_init(class: &mut Self::Class) {
            AllocationOwner::static_type();
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for DetailShowcaseView {
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for DetailShowcaseView {}
    impl BoxImpl for DetailShowcaseView {}
}

glib::wrapper! {
    pub struct DetailShowcaseView(ObjectSubclass<showcase_imp::DetailShowcaseView>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

/// How a header lays out: Home's, a detail page's or an artist sub-page's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShowcaseShape {
    /// A detail page's, with its genres and links.
    Detail,
    /// Home's, with room at the sides for ‹ ›.
    Home,
    /// The band alone.
    Plain,
}

impl ShowcaseShape {
    /// The cover's side at `width`.
    pub fn cover_size(self, width: i32) -> i32 {
        ShowcaseLayout::of(self, width).cover
    }
}

/// A header's geometry at one width.
pub(crate) struct ShowcaseLayout {
    pub(crate) band: i32,
    pub(crate) cover: i32,
    /// The cover's top edge below the band's.
    pub(crate) cover_top: i32,
    pub(crate) inset: i32,
    /// Between the cover and the text.
    pub(crate) gap: i32,
    pub(crate) size_class: &'static str,
}

impl ShowcaseLayout {
    pub(crate) fn of(shape: ShowcaseShape, width: i32) -> Self {
        let narrow = width < NARROW_WIDTH;
        let size_class = if narrow {
            "detail-showcase-small"
        } else if width < LARGE_WIDTH {
            "detail-showcase-medium"
        } else {
            "detail-showcase-large"
        };
        let (band, cover, cover_top, inset, gap) = match (shape, narrow) {
            (ShowcaseShape::Detail, false) => (196, detail_showcase_cover_size(width), 68, 24, 28),
            (ShowcaseShape::Home, false) => (
                196,
                150 + (width - NARROW_WIDTH).min(200) * 46 / 200,
                52,
                48,
                28,
            ),
            (ShowcaseShape::Detail | ShowcaseShape::Home, true) => {
                (150, detail_showcase_cover_size(width), 38, 16, 16)
            }
            (ShowcaseShape::Plain, false) => (120, 0, 0, 24, 0),
            (ShowcaseShape::Plain, true) => (104, 0, 0, 16, 0),
        };
        Self {
            band,
            cover,
            cover_top,
            inset,
            gap,
            size_class,
        }
    }
}

/// The facts and the genres as one line. The genres that do not fit it wait behind +n (› when
/// none fits), which opens them onto further lines; ‹ closes them again.
pub(crate) struct MetaLine {
    owner: AllocationOwner,
    line: adw::WrapBox,
    lead: gtk::Widget,
    more: gtk::Button,
    /// Each chip with the width it last showed with: a hidden chip measures nothing.
    chips: RefCell<Vec<(gtk::Widget, i32)>>,
    /// What `more` shows, and its width when it last showed.
    face: RefCell<String>,
    more_width: Cell<i32>,
    open: Cell<bool>,
}

const SHOW_GENRES_ICON: &str = "rufin-go-next-symbolic";
const HIDE_GENRES_ICON: &str = "rufin-go-previous-symbolic";

impl MetaLine {
    /// Binds the line in `owner`, which `lead` starts and `more` ends. `resized` runs after +n
    /// opened or closed the genres.
    pub(crate) fn bind(
        owner: &AllocationOwner,
        line: &adw::WrapBox,
        lead: &impl IsA<gtk::Widget>,
        more: &gtk::Button,
        resized: impl Fn() + 'static,
    ) -> Rc<Self> {
        let meta = Rc::new(Self {
            owner: owner.clone(),
            line: line.clone(),
            lead: lead.clone().upcast(),
            more: more.clone(),
            chips: RefCell::new(Vec::new()),
            face: RefCell::new(String::new()),
            more_width: Cell::new(0),
            open: Cell::new(false),
        });
        let weak = Rc::downgrade(&meta);
        owner.set_width_callback(Some(Rc::new(move |width| {
            if let Some(meta) = weak.upgrade() {
                meta.fit(width);
            }
        })));
        let weak = Rc::downgrade(&meta);
        more.connect_clicked(move |_| {
            let Some(meta) = weak.upgrade() else {
                return;
            };
            meta.open.set(!meta.open.get());
            meta.owner.queue_resize();
            resized();
        });
        meta
    }

    /// Shows `chips` after the facts, closed to one line.
    pub(crate) fn set_chips(&self, chips: impl IntoIterator<Item = gtk::Widget>) {
        for (chip, _) in self.chips.take() {
            self.line.remove(&chip);
        }
        let mut previous = self.lead.clone();
        let chips = chips
            .into_iter()
            .map(|chip| {
                self.line.insert_child_after(&chip, Some(&previous));
                previous = chip.clone();
                (chip, 0)
            })
            .collect();
        self.chips.replace(chips);
        self.open.set(false);
        self.owner.queue_resize();
    }

    pub(crate) fn has_chips(&self) -> bool {
        !self.chips.borrow().is_empty()
    }

    fn fit(&self, width: i32) {
        let natural = |widget: &gtk::Widget| widget.measure(gtk::Orientation::Horizontal, -1).1;
        let spacing = self.line.child_spacing();
        let mut chips = self.chips.borrow_mut();
        for (chip, shown) in chips.iter_mut() {
            if chip.is_visible() {
                *shown = natural(chip);
            }
        }
        let lead = if self.lead.is_visible() {
            natural(&self.lead).min(width) + spacing
        } else {
            0
        };
        let fits = |count: usize, reserve: i32| {
            let chips: i32 = chips[..count]
                .iter()
                .map(|(_, shown)| shown + spacing)
                .sum();
            lead + chips + reserve <= width + spacing
        };
        let all = fits(chips.len(), 0);
        if all {
            self.open.set(false);
        }
        let open = self.open.get();
        // Closed, the line stays one line: the facts give way to +n. Open, it wraps.
        self.line.set_wrap_policy(if open {
            adw::WrapPolicy::Natural
        } else {
            adw::WrapPolicy::Minimum
        });
        let count = if all || open {
            chips.len()
        } else {
            if self.more_width.get() == 0 {
                self.show_more(&format!("+{}", chips.len()));
                self.more.set_visible(true);
            }
            if self.more.is_visible() {
                self.more_width.set(natural(self.more.upcast_ref()));
            }
            let reserve = self.more_width.get() + spacing;
            (0..chips.len())
                .rev()
                .find(|count| fits(*count, reserve))
                .unwrap_or(0)
        };
        for (index, (chip, _)) in chips.iter().enumerate() {
            chip.set_visible(index < count);
        }
        if open {
            self.show_more(HIDE_GENRES_ICON);
        } else if count == 0 {
            self.show_more(SHOW_GENRES_ICON);
        } else if !all {
            self.show_more(&format!("+{}", chips.len() - count));
        }
        self.more.set_visible(!all);
    }

    /// Shows `face` on `more`: a count as its text, anything else as its icon.
    fn show_more(&self, face: &str) {
        if *self.face.borrow() == face {
            return;
        }
        if face.starts_with('+') {
            self.more.set_label(face);
        } else {
            self.more.set_icon_name(face);
        }
        self.face.replace(face.to_owned());
    }
}

/// The cover a header mounts.
pub enum ShowcaseCover {
    Single(MediaCoverProjection),
    /// A playlist's or a genre's covers; a click opens them.
    Group(CoverGroupProjection, Rc<dyn Fn()>),
}

impl ShowcaseCover {
    fn connect_texture(&self, shown: Rc<dyn Fn(&gtk::gdk::Texture)>) {
        match self {
            Self::Single(cover) => cover.tile.connect_texture(shown),
            Self::Group(cover, _) => cover.connect_texture(shown),
        }
    }

    fn resize(&self, size: i32) {
        match self {
            Self::Single(cover) => cover.resize(size),
            Self::Group(cover, _) => cover.resize(size),
        }
    }

    fn set_animated(&self) {
        match self {
            Self::Single(cover) => cover.tile.set_animated(true),
            Self::Group(cover, _) => cover.set_animated(true),
        }
    }
}

/// What a mounted header shows and does.
pub struct ShowcaseMount {
    pub shape: ShowcaseShape,
    pub initial_width: i32,
    pub cover: ShowcaseCover,
    pub play: CollectionPlay,
    /// The row's menu, from ⋯, the cover's corner and a secondary click.
    pub menu: ContextMenuOpen,
    /// Whether Download shows when the row offers it; Home's header has none.
    pub download: bool,
}

impl DetailShowcaseView {
    /// A header titled `title`; its band takes the cover's colours once the cover shows.
    pub fn new(title: &str) -> Self {
        let view: Self = glib::Object::new();
        let imp = view.imp();
        crate::favorites::install_favorite_heart(&imp.favorite);
        crate::widgets::text_slide::install(&imp.title_slide, &imp.title_viewport, &*imp.title);
        crate::widgets::text_slide::install(&imp.detail_slide, &imp.detail_viewport, &*imp.detail);
        // The facts slide as the title does when they are wider than the room.
        crate::widgets::text_slide::install(&imp.facts_slide, &imp.facts_viewport, &*imp.summary);
        let weak = view.downgrade();
        let meta = MetaLine::bind(
            &imp.meta,
            &imp.meta_line,
            &*imp.facts_fade,
            &imp.genre_more,
            move || {
                if let Some(view) = weak.upgrade() {
                    view.refit();
                }
            },
        );
        let _ = imp.meta_fit.set(meta);
        imp.band_stops
            .set(rufin_core::artwork::showcase_band(&[], 0, 0, 0));
        let weak = view.downgrade();
        imp.band.set_draw_func(move |_, cairo, width, height| {
            if let Some(view) = weak.upgrade() {
                paint_band(
                    cairo,
                    f64::from(width),
                    f64::from(height),
                    view.imp().band_stops.get(),
                );
            }
        });
        view.set_title(title);
        view
    }

    /// Takes the band's colours from `binding`'s artwork, for a header that shows no cover.
    pub(crate) fn take_band_colours_from(&self, shell: &CatalogUi, binding: ArtworkBinding) {
        let artwork = Rc::clone(&shell.artwork);
        let view = self.downgrade();
        glib::MainContext::ref_thread_default().spawn_local(async move {
            if let Some(texture) = artwork.load_texture(binding, 64).await
                && let Some(view) = view.upgrade()
            {
                view.take_band_colours(&texture);
            }
        });
    }

    /// Takes the band's colours from the texture the cover shows.
    fn take_band_colours(&self, texture: &gtk::gdk::Texture) {
        let stops = band_stops(texture);
        let imp = self.imp();
        if imp.band_stops.replace(stops) != stops {
            imp.band.queue_draw();
            if let Some(strip) = &*imp.strip.borrow() {
                strip.band.queue_draw();
            }
        }
    }

    pub fn set_title(&self, text: &str) {
        let imp = self.imp();
        imp.title.set_text(text);
        if let Some(strip) = &*imp.strip.borrow() {
            strip.title.set_text(text);
        }
        self.refit();
    }

    /// Makes `page`, which holds the mounted header above the list in `list`, scroll as one
    /// with that list. As the header leaves, a strip with the title and Play fades in; the
    /// page goes on leaving until `frame` (the list with its column header) is under it.
    pub fn scrolling_page(
        &self,
        page: &impl IsA<gtk::Widget>,
        (list, frame): (gtk::ScrolledWindow, gtk::Widget),
    ) -> gtk::Widget {
        let imp = self.imp();
        let weak = self.downgrade();
        let strip = DetailStrip::new(
            move || weak.upgrade().map(|view| view.imp().band_stops.get()),
            &imp.play,
        );
        strip.title.set_text(&imp.title.text());
        // A header without buttons has a strip without Play.
        strip.play.set_visible(imp.actions.is_visible());
        let scrolling = super::detail_scroll::detail_scroll(
            page,
            &*imp.body,
            &strip.root,
            Some((&list, &frame)),
        );
        imp.strip.replace(Some(strip));
        scrolling
    }

    /// Shows `text` under the title: an album's artist, or the sub-page an artist's band heads.
    pub fn set_detail(&self, text: &str) -> gtk::Label {
        let imp = self.imp();
        imp.detail.set_text(text);
        imp.detail_fade.set_visible(true);
        imp.detail.clone()
    }

    /// Adds a fact after the summary's: an artist's album and track counts.
    pub fn append_fact(&self, child: &impl IsA<gtk::Widget>) {
        // Buttons (an artist's counts) carry their own boxes; the row around them takes none.
        self.imp().facts_fade.remove_css_class("detail-facts-boxed");
        self.imp().summary.append(child);
        self.imp().has_facts.set(true);
        self.refit();
    }

    pub fn replace_summary(&self, values: &[(&str, String)]) {
        let imp = self.imp();
        let items = [
            (
                &imp.summary_item_0,
                &imp.summary_icon_0,
                &imp.summary_label_0,
            ),
            (
                &imp.summary_item_1,
                &imp.summary_icon_1,
                &imp.summary_label_1,
            ),
            (
                &imp.summary_item_2,
                &imp.summary_icon_2,
                &imp.summary_label_2,
            ),
        ];
        let mut shown_before = false;
        for (index, (item, icon, label)) in items.into_iter().enumerate() {
            let value = values
                .get(index)
                .filter(|(_, text)| summary_value_is_visible(text));
            if let Some((icon_name, text)) = value {
                icon.set_icon_name(Some(icon_name));
                label.set_text(text);
            }
            item.set_visible(value.is_some());
            // A fact after a shown one is divided from it (on Home's band, by a |).
            if shown_before {
                item.add_css_class("detail-fact-follows");
            } else {
                item.remove_css_class("detail-fact-follows");
            }
            shown_before |= value.is_some();
        }
        if shown_before {
            imp.facts_fade.add_css_class("detail-facts-boxed");
        } else {
            imp.facts_fade.remove_css_class("detail-facts-boxed");
        }
        imp.has_facts.set(shown_before);
        self.refit();
    }

    pub fn bind_summary_text_with(&self, index: usize, text: impl Fn() -> String + 'static) {
        let imp = self.imp();
        let labels = [
            &imp.summary_label_0,
            &imp.summary_label_1,
            &imp.summary_label_2,
        ];
        if let Some(label) = labels.get(index) {
            bind_label_text_with(label, text);
        }
    }

    /// Shows `links` in the band's corner: the external sites, or the playlist's source.
    pub fn replace_external_links(&self, links: Option<gtk::Widget>) {
        let host = &self.imp().external_links;
        while let Some(child) = host.first_child() {
            host.remove(&child);
        }
        host.set_visible(links.is_some());
        if let Some(links) = links {
            host.append(&links);
        }
    }

    /// Shows `genres` as chips after the facts; those that do not fit their line go under +n.
    pub(crate) fn set_genres<'a>(
        &self,
        shell: &Rc<CatalogUi>,
        genres: impl IntoIterator<Item = (&'a str, library::GenreKey)>,
    ) {
        let chips = genres
            .into_iter()
            .map(|(name, key)| (name.trim(), key))
            .filter(|(name, _)| !name.is_empty())
            .map(|(name, key)| {
                let chip = genre_chip(name);
                let shell = Rc::downgrade(shell);
                chip.connect_clicked(move |_| {
                    if let Some(shell) = shell.upgrade() {
                        shell.open_route(Route::GenreDetail(key));
                    }
                });
                chip.upcast()
            })
            .collect::<Vec<gtk::Widget>>();
        if let Some(meta) = self.imp().meta_fit.get() {
            meta.set_chips(chips);
        }
        self.refit();
    }

    /// Mounts the header for `row`: its cover, with a playlist's picture buttons on hover, its
    /// buttons, and its shape at each width.
    pub(crate) fn mount(
        &self,
        shell: &Rc<CatalogUi>,
        row: Row,
        mount: ShowcaseMount,
    ) -> gtk::Widget {
        let ShowcaseMount {
            shape,
            initial_width,
            cover,
            play,
            menu,
            download,
        } = mount;
        cover.set_animated();
        let weak = self.downgrade();
        cover.connect_texture(Rc::new(move |texture| {
            if let Some(view) = weak.upgrade() {
                view.take_band_colours(texture);
            }
        }));
        let button = match &cover {
            ShowcaseCover::Single(cover) => cover.button(),
            ShowcaseCover::Group(cover, open) => {
                let button = detail_cover_button(&cover.widget());
                let open = Rc::clone(open);
                button.connect_clicked(move |_| open());
                button
            }
        };
        let overlay = gtk::Overlay::new();
        overlay.add_css_class("cover-frame");
        overlay.set_halign(gtk::Align::Start);
        overlay.set_valign(gtk::Align::Start);
        overlay.set_child(Some(&button));
        install_context_menu_openers(&overlay, Rc::clone(&menu));
        // Home's showcase steps through albums; a page's own cover changes its picture.
        if shape != ShowcaseShape::Home {
            self.install_artwork_button(&overlay);
        }
        self.imp().cover_host.append(&overlay);
        self.bind_actions(shell, play, menu, download);
        self.show_row(row);
        self.present(shape, initial_width, Some(cover))
    }

    /// Mounts a header with only its band: an artist's sub-page.
    pub fn mount_plain(&self, initial_width: i32) -> gtk::Widget {
        let imp = self.imp();
        imp.cover_host.set_visible(false);
        imp.actions.set_visible(false);
        self.present(ShowcaseShape::Plain, initial_width, None)
    }

    /// Shows `row`'s radio, favorite and downloads in the buttons, which then act on it.
    pub fn show_row(&self, row: Row) {
        let imp = self.imp();
        let Some(shell) = imp.shell.borrow().upgrade() else {
            return;
        };
        let offered = rufin_core::menu::swipes(
            &shell.products.source,
            &shell.settings.current.borrow(),
            &row,
        );
        imp.radio.set_visible(offered.radio.is_some());
        imp.favorite.set_visible(offered.favorite);
        if let Some((_, favorite)) = row_favorite(&row) {
            set_favorite_button_active(&imp.favorite, favorite);
        }
        imp.edit
            .set_visible(offered.edit && playlist_row(&row).is_some());
        if imp.artwork_button.borrow().is_some() {
            self.show_artwork_writable(&shell, row.clone(), offered.edit_metadata);
        }
        let downloaded = offered.remove_download;
        imp.download
            .set_visible(imp.offers_download.get() && (offered.download || downloaded));
        if downloaded {
            imp.download.add_css_class("active-toggle");
        } else {
            imp.download.remove_css_class("active-toggle");
        }
        imp.download.set_tooltip_text(Some(&tr(if downloaded {
            msgid("Remove Downloads")
        } else {
            msgid("Download")
        })));
        imp.row.replace(Some(row));
        self.refit();
    }

    /// Lays the header out again at its last width, after its text changed.
    fn refit(&self) {
        if let Some((shape, width)) = self.imp().fitted.get() {
            self.apply_layout(&ShowcaseLayout::of(shape, width), shape, width);
        }
    }

    /// The band tall enough that the title, its line, the facts and the genres end above its
    /// edge.
    fn fit_text(&self, layout: &ShowcaseLayout, width: i32) {
        let imp = self.imp();
        let text_width = (width
            - imp.body.margin_start()
            - imp.body.margin_end()
            - layout.cover
            - imp.body.spacing())
        .max(1);
        let text = imp.names.measure(gtk::Orientation::Vertical, text_width).1;
        let band = layout.band.max(text + 12 + 16);
        imp.band.set_height_request(band);
        imp.heading.set_height_request(band - 12);
        let step_top = (band - STEP_BUTTON_SIZE) / 2;
        imp.step_previous.set_margin_top(step_top);
        imp.step_next.set_margin_top(step_top);
    }

    /// Shows ‹ › at the band's sides while the pointer is over the header, and steps on a
    /// horizontal touch swipe; both call `step` with whether they go forward.
    pub fn enable_steps(&self, step: Rc<dyn Fn(bool)>) {
        let imp = self.imp();
        for (button, forward) in [(&*imp.step_previous, false), (&*imp.step_next, true)] {
            let step = Rc::clone(&step);
            button.connect_clicked(move |_| step(forward));
        }
        let buttons = [imp.step_previous.downgrade(), imp.step_next.downgrade()];
        let reveal = move |shown: bool| {
            for button in buttons.iter().filter_map(glib::WeakRef::upgrade) {
                button.set_visible(shown);
            }
        };
        let motion = gtk::EventControllerMotion::new();
        let enter = reveal.clone();
        motion.connect_enter(move |_, _, _| enter(true));
        motion.connect_leave(move |_| reveal(false));
        imp.layers.add_controller(motion);
        // A touch drag that is mostly sideways is the header's; a vertical one scrolls Home.
        let swipe = gtk::GestureDrag::new();
        swipe.set_touch_only(true);
        swipe.set_propagation_phase(gtk::PropagationPhase::Capture);
        let sideways = Rc::new(Cell::new(false));
        let decided = Rc::clone(&sideways);
        swipe.connect_drag_begin(move |_, _, _| decided.set(false));
        let decided = Rc::clone(&sideways);
        swipe.connect_drag_update(move |swipe, dx, dy| {
            if !decided.get() && dx.abs().max(dy.abs()) > 12.0 {
                if dx.abs() > dy.abs() * 1.5 {
                    decided.set(true);
                    swipe.set_state(gtk::EventSequenceState::Claimed);
                } else {
                    swipe.set_state(gtk::EventSequenceState::Denied);
                }
            }
        });
        swipe.connect_drag_end(move |_, dx, _| {
            if sideways.get() && dx.abs() > 48.0 {
                step(dx < 0.0);
            }
        });
        imp.layers.add_controller(swipe);
    }

    fn bind_actions(
        &self,
        shell: &Rc<CatalogUi>,
        play: CollectionPlay,
        menu: ContextMenuOpen,
        download: bool,
    ) {
        let imp = self.imp();
        imp.shell.replace(Rc::downgrade(shell));
        imp.offers_download.set(download);
        imp.play
            .connect_clicked(move |_| play(QueuePlacement::Now, false));
        // The menu hangs from the button row under ⋯: a popover is no child for a button.
        let row = imp.actions.downgrade();
        imp.more.connect_clicked(move |button| {
            let Some(row) = row.upgrade() else {
                return;
            };
            let under = gtk::graphene::Point::new(0.0, button.height() as f32);
            let point = button
                .compute_point(&row, &under)
                .map(|point| (f64::from(point.x()), f64::from(point.y())));
            menu(row.upcast_ref(), point);
        });
        let view = self.downgrade();
        imp.radio.connect_clicked(move |_| {
            let Some((shell, row)) = view.upgrade().and_then(|view| view.shown()) else {
                return;
            };
            if let Some(seed) = rufin_core::menu::radio_seed(&row) {
                shell
                    .products
                    .playback
                    .play_radio(RadioPlayRequest::now(seed));
            }
        });
        let view = self.downgrade();
        imp.favorite.connect_clicked(move |button| {
            let Some((shell, row)) = view.upgrade().and_then(|view| view.shown()) else {
                return;
            };
            if let Some((target, _)) = row_favorite(&row) {
                shell.set_favorite(target, !favorite_button_is_active(button));
            }
        });
        let view = self.downgrade();
        imp.edit.connect_clicked(move |_| {
            let Some((shell, row)) = view.upgrade().and_then(|view| view.shown()) else {
                return;
            };
            match playlist_row(&row) {
                Some(PlaylistListRow::Playlist(playlist)) => shell
                    .shell()
                    .edit_playlist_dialog(playlist.playlist_key, playlist.name.clone()),
                Some(PlaylistListRow::SmartPlaylist(playlist)) => {
                    shell.shell().edit_smart_playlist_dialog(playlist.clone())
                }
                _ => {}
            }
        });
        let view = self.downgrade();
        imp.download.connect_clicked(move |button| {
            let Some((shell, row)) = view.upgrade().and_then(|view| view.shown()) else {
                return;
            };
            if let Some(target) = rufin_core::view::PlayTarget::of_row(&row) {
                crate::menus::media_menus::download(
                    shell.shell(),
                    &MediaDragSource::capture_target(target),
                    button.has_css_class("active-toggle"),
                );
            }
        });
    }

    /// Puts the Artwork button on the cover; it shows under the pointer while the row's picture
    /// can change, and opens the editor that changes it.
    fn install_artwork_button(&self, overlay: &gtk::Overlay) {
        let resource = "routes/cover_artwork.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, { edit_artwork: gtk::Button });
        overlay.add_overlay(&edit_artwork);
        let view = self.downgrade();
        edit_artwork.connect_clicked(move |_| {
            if let Some((shell, row)) = view.upgrade().and_then(|view| view.shown()) {
                shell.shell().edit_row_artwork(row);
            }
        });
        let (view, button) = (self.downgrade(), edit_artwork.downgrade());
        let show = Rc::new(move |shown: bool| {
            if let (Some(view), Some(button)) = (view.upgrade(), button.upgrade()) {
                button.set_visible(shown && view.imp().artwork_writable.get());
            }
        });
        let motion = gtk::EventControllerMotion::new();
        let entered = Rc::clone(&show);
        motion.connect_enter(move |_, _, _| entered(true));
        motion.connect_leave(move |_| show(false));
        overlay.add_controller(motion);
        self.imp().artwork_button.replace(Some(edit_artwork));
    }

    /// Reads whether `row`'s cover can change its picture.
    fn show_artwork_writable(&self, shell: &CatalogUi, row: Row, edit_metadata: bool) {
        self.set_artwork_writable(false);
        let view = self.downgrade();
        let shell = Rc::clone(shell.shell());
        glib::spawn_future_local(async move {
            let writable =
                crate::playlists::artwork::artwork_writable(&shell, &row, edit_metadata).await;
            // A later row's own read decides for it.
            if let Some(view) = view.upgrade()
                && view.row().is_some_and(|shown| shown == row)
            {
                view.set_artwork_writable(writable);
            }
        });
    }

    fn set_artwork_writable(&self, writable: bool) {
        let imp = self.imp();
        imp.artwork_writable.set(writable);
        if !writable && let Some(button) = imp.artwork_button.borrow().as_ref() {
            button.set_visible(false);
        }
    }

    /// The row the header shows.
    pub fn row(&self) -> Option<Row> {
        self.imp().row.borrow().clone()
    }

    /// The page and the row the buttons act on.
    fn shown(&self) -> Option<(Rc<CatalogUi>, Row)> {
        let imp = self.imp();
        Some((imp.shell.borrow().upgrade()?, imp.row.borrow().clone()?))
    }

    fn present(
        &self,
        shape: ShowcaseShape,
        initial_width: i32,
        cover: Option<ShowcaseCover>,
    ) -> gtk::Widget {
        let view = self.clone();
        let shown = Cell::new(0);
        let apply = move |width: i32| {
            if width <= 1 || shown.replace(width) == width {
                return;
            }
            let layout = ShowcaseLayout::of(shape, width);
            view.apply_layout(&layout, shape, width);
            if let Some(cover) = &cover {
                cover.resize(layout.cover);
            }
        };
        apply(initial_width);
        width_allocation_owner(self, apply).upcast()
    }

    fn apply_layout(&self, layout: &ShowcaseLayout, shape: ShowcaseShape, width: i32) {
        let imp = self.imp();
        for class in [
            "detail-showcase-small",
            "detail-showcase-medium",
            "detail-showcase-large",
        ] {
            if class == layout.size_class {
                self.add_css_class(class);
            } else {
                self.remove_css_class(class);
            }
        }
        imp.band.set_height_request(layout.band);
        let has_facts = imp.has_facts.get();
        let has_genres = imp.meta_fit.get().is_some_and(|meta| meta.has_chips());
        imp.facts_fade.set_visible(has_facts);
        imp.meta
            .set_visible(shape != ShowcaseShape::Plain && (has_facts || has_genres));
        imp.cover_host.set_margin_top(layout.cover_top);
        // The names end just above the band's edge, and the buttons start just under it.
        imp.heading.set_height_request(layout.band - 12);
        let step_top = (layout.band - STEP_BUTTON_SIZE) / 2;
        imp.step_previous.set_margin_top(step_top);
        imp.step_next.set_margin_top(step_top);
        // Room under a cover that hangs over the band's edge, so its shadow is not cut.
        imp.body
            .set_margin_bottom(if layout.cover != 0 { 28 } else { 0 });
        // Play, then Radio, give up their labels while the buttons do not fit their room, so the
        // row stays one row with ⋯ in view.
        let room = width - 2 * layout.inset - layout.cover - layout.gap;
        let pills = [
            (&*imp.play, &*imp.play_text, &imp.labelled_play),
            (&*imp.radio, &*imp.radio_text, &imp.labelled_radio),
        ];
        // The row's width with both labels, and what each pill saves as a circle.
        let mut labelled_width = imp.actions.measure(gtk::Orientation::Horizontal, -1).1;
        let mut savings = [0; 2];
        for (index, (button, text, labelled)) in pills.iter().enumerate() {
            if !button.is_visible() {
                continue;
            }
            let current = button.measure(gtk::Orientation::Horizontal, -1).1;
            if text.is_visible() {
                labelled.set(current);
            }
            labelled_width += labelled.get() - current;
            savings[index] = labelled.get() - button.measure(gtk::Orientation::Vertical, -1).1;
        }
        let play_labelled = labelled_width <= room;
        let radio_labelled = play_labelled || labelled_width - savings[0] <= room;
        // Where even the circles do not fit, the insets and the gap beside the cover give way
        // together, so the row stays one row beside a cover no narrower than its hover controls.
        let overflow = (labelled_width - savings[0] - savings[1] - room).max(0);
        let (inset, gap) = if overflow == 0 {
            (layout.inset, layout.gap)
        } else {
            let spacing = (2 * layout.inset + layout.gap - overflow).max(0) / 3;
            (spacing, spacing)
        };
        imp.body.set_spacing(gap);
        imp.body.set_margin_start(inset);
        imp.body.set_margin_end(inset);
        for ((button, text, _), labelled) in pills.into_iter().zip([play_labelled, radio_labelled])
        {
            text.set_visible(labelled);
            if labelled {
                button.remove_css_class("detail-pill-icon");
            } else {
                button.add_css_class("detail-pill-icon");
            }
        }
        let tooltip = tr("Play");
        imp.play
            .set_tooltip_text((!play_labelled).then_some(tooltip.as_str()));
        imp.fitted.set(Some((shape, width)));
        self.fit_text(layout, width);
    }
}

/// A header as a strip, for a page that scrolls its header away: the header's title on its
/// band, with its Play.
pub(crate) struct DetailStrip {
    pub(crate) root: gtk::Widget,
    pub(crate) band: gtk::DrawingArea,
    pub(crate) title: gtk::Label,
    pub(crate) play: gtk::Button,
    pub(crate) play_icon: gtk::Image,
    pub(crate) play_text: gtk::Label,
}

impl DetailStrip {
    /// A strip whose band paints the header's colours `stops`, and whose Play clicks the
    /// header's own, `header_play`.
    pub(crate) fn new(
        stops: impl Fn() -> Option<[[u8; 3]; 3]> + 'static,
        header_play: &gtk::Button,
    ) -> Self {
        let resource = "routes/detail_strip.ui";
        let builder = crate::ui_resource::builder(resource);
        crate::objects!(builder, resource, {
            root: gtk::Overlay,
            band: gtk::DrawingArea,
            title_host: gtk::Box,
            play: gtk::Button,
            play_icon: gtk::Image,
            play_text: gtk::Label,
        });
        root.set_margin_top(ROUTE_TOP_MARGIN);
        root.set_margin_bottom(ROUTE_TOP_MARGIN);
        root.set_margin_start(PRIMARY_ROUTE_MARGIN_START);
        root.set_margin_end(PRIMARY_ROUTE_MARGIN_END);
        band.set_draw_func(move |_, cairo, width, height| {
            if let Some(stops) = stops() {
                paint_band(cairo, f64::from(width), f64::from(height), stops);
            }
        });
        let (slide, title) = crate::widgets::text_slide::new_label();
        title.add_css_class("detail-title");
        title_host.append(&slide);
        let header_play = header_play.downgrade();
        play.connect_clicked(move |_| {
            if let Some(header_play) = header_play.upgrade() {
                header_play.emit_clicked();
            }
        });
        Self {
            root: root.upcast(),
            band,
            title,
            play,
            play_icon,
            play_text,
        }
    }
}

/// The design's band: a 135° gradient through the cover's colours `stops`, darkening toward
/// the edge the cover overlaps.
pub(crate) fn paint_band(
    cairo: &gtk::cairo::Context,
    width: f64,
    height: f64,
    stops: [[u8; 3]; 3],
) {
    use std::f64::consts::{FRAC_PI_2, PI};
    let radius = 8.0_f64.min(height / 2.0);
    cairo.new_sub_path();
    cairo.arc(width - radius, radius, radius, -FRAC_PI_2, 0.0);
    cairo.arc(width - radius, height - radius, radius, 0.0, FRAC_PI_2);
    cairo.arc(radius, height - radius, radius, FRAC_PI_2, PI);
    cairo.arc(radius, radius, radius, PI, 3.0 * FRAC_PI_2);
    cairo.close_path();
    // CSS places a 135° gradient's ends so the corners are its 0 % and 100 %.
    let reach = (width + height) / 4.0;
    let (x, y) = (width / 2.0, height / 2.0);
    let diagonal = gtk::cairo::LinearGradient::new(x - reach, y - reach, x + reach, y + reach);
    for (offset, [r, g, b]) in [0.0, 0.58, 1.0].into_iter().zip(stops) {
        diagonal.add_color_stop_rgb(
            offset,
            f64::from(r) / 255.0,
            f64::from(g) / 255.0,
            f64::from(b) / 255.0,
        );
    }
    let shade = gtk::cairo::LinearGradient::new(0.0, 0.0, 0.0, height);
    shade.add_color_stop_rgba(0.3, 0.0, 0.0, 0.0, 0.0);
    shade.add_color_stop_rgba(1.0, 0.0, 0.0, 0.0, 0.3);
    for source in [&diagonal, &shade] {
        if cairo.set_source(source).is_err() || cairo.fill_preserve().is_err() {
            return;
        }
    }
    cairo.new_path();
}

/// A band's colours from the texture its cover shows.
pub(crate) fn band_stops(texture: &gtk::gdk::Texture) -> [[u8; 3]; 3] {
    let mut downloader = gtk::gdk::TextureDownloader::new(texture);
    downloader.set_format(gtk::gdk::MemoryFormat::R8g8b8a8);
    let (pixels, stride) = downloader.download_bytes();
    rufin_core::artwork::showcase_band(
        &pixels,
        texture.width().max(0) as u32,
        texture.height().max(0) as u32,
        stride as u32,
    )
}

fn summary_value_is_visible(text: &str) -> bool {
    !text.is_empty()
}

fn genre_chip(label: &str) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    button.add_css_class("flat");
    button.add_css_class("detail-genre-chip");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(label));
    button
}

#[derive(Clone)]
pub struct MediaCoverProjection {
    button: gtk::Button,
    tile: ArtworkTile,
    size: Rc<Cell<i32>>,
    candidates: Rc<RefCell<ArtworkBinding>>,
}

impl MediaCoverProjection {
    pub fn button(&self) -> gtk::Button {
        self.button.clone()
    }

    pub fn drag_paintable_source(&self) -> gtk::Picture {
        self.tile.drag_paintable_source()
    }

    pub fn resize(&self, size: i32) {
        let size = size.max(1);
        if self.size.replace(size) == size {
            return;
        }
        self.button.set_size_request(size, size);
        self.tile.set_square_size(size);
    }

    pub(crate) fn replace(&self, shell: &Rc<CatalogUi>, candidates: ArtworkBinding) {
        self.candidates.replace(candidates.clone());
        shell
            .artwork
            .bind_artwork_tile(&self.tile, candidates, detail_cover_render_size());
    }
}

pub fn media_cover_projection(
    shell: &Rc<CatalogUi>,
    candidates: ArtworkBinding,
    size: i32,
    cover_class: &str,
) -> MediaCoverProjection {
    media_cover_projection_with_open(shell, candidates, size, cover_class, |shell, candidates| {
        shell.present_full_artwork(candidates)
    })
}

pub fn home_album_cover_projection(
    shell: &Rc<CatalogUi>,
    album: &AlbumRow,
    size: i32,
) -> MediaCoverProjection {
    let album_uri = album.media_uri.clone();
    media_cover_projection_with_open(
        shell,
        album
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        size,
        "",
        move |shell, _| shell.open_route(Route::AlbumDetail(album_uri.clone())),
    )
}

fn media_cover_projection_with_open(
    shell: &Rc<CatalogUi>,
    candidates: ArtworkBinding,
    size: i32,
    cover_class: &str,
    open: impl Fn(&Rc<CatalogUi>, ArtworkBinding) + 'static,
) -> MediaCoverProjection {
    let render_size = detail_cover_render_size();
    let tile = ArtworkTile::new_sized(size, size);
    shell
        .artwork
        .bind_artwork_tile(&tile, candidates.clone(), render_size);
    let cover = tile.widget();
    cover.add_css_class("detail-showcase-cover");
    if !cover_class.is_empty() {
        cover.add_css_class(cover_class);
    }

    let button = detail_cover_button(&cover);

    let candidates = Rc::new(RefCell::new(candidates));
    let open_candidates = Rc::clone(&candidates);
    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| {
        let candidates = open_candidates.borrow().clone();
        open(&shell, candidates);
    });
    MediaCoverProjection {
        button,
        tile,
        size: Rc::new(Cell::new(size)),
        candidates,
    }
}

/// The playlist or smart playlist `row` is, whose picture and settings its page edits.
fn playlist_row(row: &Row) -> Option<&PlaylistListRow> {
    match row {
        Row::Playlist(
            playlist @ (PlaylistListRow::Playlist(_) | PlaylistListRow::SmartPlaylist(_)),
        ) => Some(playlist),
        _ => None,
    }
}

fn detail_cover_render_size() -> i32 {
    detail_showcase_cover_size(i32::MAX)
}

fn detail_cover_button(cover: &gtk::Widget) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("detail-cover-button");
    button.set_halign(gtk::Align::Start);
    button.set_valign(gtk::Align::Start);
    button.set_cursor_from_name(Some("pointer"));
    button.set_child(Some(cover));
    button
}

/// A playlist's source at the end of its facts: a link to it on its server, or its icon.
pub fn source_link(shell: &CatalogUi, icon: &str, name: &str, uri: Option<String>) -> gtk::Widget {
    let row = detail_external_link_row();
    let button = detail_link_button(icon, name);
    button.update_property(&[gtk::accessible::Property::Label(name)]);
    match uri {
        Some(uri) => connect_web_link(&button, shell.chrome.window.downgrade(), uri),
        None => button.set_can_target(false),
    }
    row.append(&button);
    row.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsive_detail_artwork_covers_every_presented_size() {
        let render = detail_cover_render_size();
        for shape in [ShowcaseShape::Detail, ShowcaseShape::Home] {
            for width in [240, 360, 520, 560, 760, 1_200, i32::MAX] {
                assert!(render >= shape.cover_size(width));
            }
        }
    }

    #[test]
    fn empty_summary_values_hide_the_icon_and_text() {
        assert!(!summary_value_is_visible(""));
        assert!(summary_value_is_visible("0"));
    }
}

pub fn album_external_links(shell: &Rc<CatalogUi>, album: &AlbumView) -> Option<gtk::Widget> {
    let settings = shell.settings.current.borrow();
    let source = library::source_entity_parts(&album.row.media_uri)
        .and_then(|(source_id, _, _)| shell.products.source.configuration(&source_id));
    detail_external_links(
        shell,
        rufin_core::detail_links::album_external_links(
            &settings.application.external_site_links,
            source.as_ref(),
            album,
        ),
        &album.row.media_uri,
        false,
    )
}

pub fn artist_external_links(
    shell: &Rc<CatalogUi>,
    artist: &ArtistView,
    album_artist: bool,
) -> Option<gtk::Widget> {
    let settings = shell.settings.current.borrow();
    let source = library::source_entity_parts(&artist.row.media_uri)
        .and_then(|(source_id, _, _)| shell.products.source.configuration(&source_id));
    detail_external_links(
        shell,
        rufin_core::detail_links::artist_external_links(
            &settings.application.external_site_links,
            source.as_ref(),
            artist,
        ),
        &artist.row.media_uri,
        album_artist,
    )
}

fn detail_external_links(
    shell: &Rc<CatalogUi>,
    links: Vec<rufin_core::detail_links::DetailLink>,
    media_uri: &str,
    album_artist: bool,
) -> Option<gtk::Widget> {
    let row = detail_external_link_row();
    for link in links {
        if let Some(url) = link.url {
            row.append(&detail_external_link_button(
                shell,
                link.icon_name,
                link.title,
                url,
            ));
        } else if let Some(button) = detail_source_button(shell, media_uri, album_artist) {
            row.append(&button);
        }
    }
    row.first_child().is_some().then(|| row.upcast())
}

fn detail_external_link_row() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("detail-external-link-row");
    row
}

fn detail_external_link_button(
    shell: &Rc<CatalogUi>,
    icon_name: &str,
    label: &str,
    url: String,
) -> gtk::Button {
    let button = detail_link_button(icon_name, label);
    connect_web_link(&button, shell.chrome.window.downgrade(), url);
    button
}

fn connect_web_link(
    button: &gtk::Button,
    window: glib::WeakRef<gtk::ApplicationWindow>,
    url: String,
) {
    button.connect_clicked(move |_| {
        if let Some(window) = window.upgrade() {
            open_web_link(&window, &url);
        }
    });
}

/// Opens `url` in the browser.
pub(crate) fn open_web_link(window: &gtk::ApplicationWindow, url: &str) {
    let launcher = gtk::UriLauncher::new(url);
    let window = window.clone();
    gtk::glib::spawn_future_local(async move {
        if let Err(error) = launcher.launch_future(Some(&window)).await {
            warn!(%error, "failed to open external detail link");
        }
    });
}

fn detail_link_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("detail-external-link-button");
    button.set_tooltip_text(Some(&tr(label)));
    let image = gtk::Image::from_icon_name(icon_name);
    image.set_pixel_size(18);
    button.set_child(Some(&image));
    button
}

fn detail_source_button(
    shell: &CatalogUi,
    media_uri: &str,
    album_artist: bool,
) -> Option<gtk::Button> {
    let (source_id, _, _) = library::source_entity_parts(media_uri)?;
    let source = shell.products.source.configuration(&source_id)?;
    let link = rufin_core::detail_links::source_link(&source, media_uri)?;
    let button = detail_link_button(link.icon_name, link.title);
    if let Some(uri) = link.url {
        connect_web_link(&button, shell.chrome.window.downgrade(), uri);
        return Some(button);
    }
    let source = shell.products.source.clone();
    let window = shell.chrome.window.downgrade();
    let media_uri = media_uri.to_owned();
    button.connect_clicked(move |button| {
        let Some(window) = window.upgrade() else {
            return;
        };
        button.set_sensitive(false);
        let button = button.downgrade();
        let receiver = source.collection_folder_uri(media_uri.clone(), album_artist);
        glib::spawn_future_local(async move {
            let result = async {
                let uri = receiver.recv().await.map_err(|error| error.to_string())??;
                crate::widgets::folder_launcher::open_folder_uri(&window, &uri).await
            }
            .await;
            if let Some(button) = button.upgrade() {
                button.set_sensitive(true);
            }
            if let Err(error) = result {
                warn!(%error, "failed to open source detail location");
            }
        });
    });
    Some(button)
}
