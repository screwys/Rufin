use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
};

use ::library::{AlbumRow, AlbumView, ArtistView, Row};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};
use localization::{msgid, tr};
use rufin_core::playback::{QueuePlacement, RadioPlayRequest};
use tracing::warn;

use crate::favorites::{favorite_button_is_active, set_favorite_button_active};
use crate::library::CatalogUi;
use crate::library::route_layout::{
    DETAIL_SHOWCASE_STACKED_WIDTH as STACKED_WIDTH, detail_showcase_cover_size,
};
use crate::menus::media_menus::{CollectionPlay, row_favorite};
use crate::widgets::artwork::{ArtworkTile, CoverGroupProjection};
use crate::widgets::cover_controls::{
    cover_hover_controls_with_favorite, cover_hover_min_width, showcase_cover_overlay,
};
use crate::widgets::interactions::ContextMenuOpen;
use crate::widgets::layout::{AllocationOwner, width_allocation_owner};
use crate::widgets::localization::bind_label_text_with;
use crate::widgets::media_drag::MediaDragSource;
use rufin_core::route::Route;

/// From this width titles take their largest size.
const LARGE_WIDTH: i32 = 840;
const GENRE_SPACING: i32 = 6;
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
        pub details: TemplateChild<gtk::Box>,
        #[template_child]
        pub below: TemplateChild<gtk::Box>,
        #[template_child]
        pub facts: TemplateChild<gtk::Box>,
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
        pub genres: TemplateChild<AllocationOwner>,
        #[template_child]
        pub genre_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub genre_overflow: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub genre_overflow_list: TemplateChild<gtk::Box>,
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
        /// The favorite in the cover's corner on hover; it toggles the same favorite as ♡.
        pub cover_favorite: RefCell<Option<gtk::Button>>,
        /// Play's and Radio's widths with their labels, to tell whether the labels fit again.
        pub labelled_play: Cell<i32>,
        pub labelled_radio: Cell<i32>,
        /// The band's gradient stops, from the cover once it shows.
        pub band_stops: Cell<[[u8; 3]; 3]>,
        /// The shape and width last laid out, to fit the text again when it changes.
        pub fitted: Cell<Option<(ShowcaseShape, i32)>>,
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
    /// Facts and genres under the band; the cover takes its own row when narrow.
    Detail,
    /// Facts on the band and the buttons beside the cover, at every width.
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
struct ShowcaseLayout {
    /// The cover in its own row, centred, with the text under it.
    stacked: bool,
    band: i32,
    cover: i32,
    /// The cover's top edge below the band's.
    cover_top: i32,
    inset: i32,
    /// Between the cover and the text.
    gap: i32,
    size_class: &'static str,
}

impl ShowcaseLayout {
    fn of(shape: ShowcaseShape, width: i32) -> Self {
        let narrow = width < STACKED_WIDTH;
        let size_class = if narrow {
            "detail-showcase-small"
        } else if width < LARGE_WIDTH {
            "detail-showcase-medium"
        } else {
            "detail-showcase-large"
        };
        let (stacked, band, cover, cover_top, inset, gap) = match (shape, narrow) {
            (ShowcaseShape::Detail, false) => {
                (false, 196, detail_showcase_cover_size(width), 68, 24, 28)
            }
            (ShowcaseShape::Detail, true) => {
                (true, 136, detail_showcase_cover_size(width), 24, 16, 22)
            }
            // Home leaves room at the sides for ‹ ›.
            (ShowcaseShape::Home, false) => (
                false,
                196,
                150 + (width - STACKED_WIDTH).min(200) * 46 / 200,
                52,
                48,
                28,
            ),
            (ShowcaseShape::Home, true) => (
                false,
                150,
                // Never narrower than its hover controls.
                ((width - 32) * 3 / 10).clamp(cover_hover_min_width(), 150),
                38,
                16,
                16,
            ),
            (ShowcaseShape::Plain, false) => (false, 120, 0, 0, 24, 0),
            (ShowcaseShape::Plain, true) => (false, 104, 0, 0, 16, 0),
        };
        Self {
            stacked,
            band,
            cover,
            cover_top,
            inset,
            gap,
            size_class,
        }
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
}

/// What a mounted header shows and does.
pub struct ShowcaseMount {
    pub shape: ShowcaseShape,
    pub initial_width: i32,
    pub cover: ShowcaseCover,
    /// The cover's hover Play: "Play album".
    pub play_label: &'static str,
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
        crate::widgets::text_slide::install(&imp.title_slide, &imp.title_viewport, &*imp.title);
        // The facts slide as the title does when they are wider than the room.
        crate::widgets::text_slide::install(&imp.facts_slide, &imp.facts_viewport, &*imp.summary);
        imp.band_stops.set(::artwork::showcase_band(&[], 0, 0, 0));
        let weak = view.downgrade();
        imp.band.set_draw_func(move |_, cairo, width, height| {
            if let Some(view) = weak.upgrade() {
                view.paint_band(cairo, f64::from(width), f64::from(height));
            }
        });
        view.set_title(title);
        view
    }

    /// The design's band: a 135° gradient through the cover's colours, darkening toward the
    /// edge the cover overlaps.
    fn paint_band(&self, cairo: &gtk::cairo::Context, width: f64, height: f64) {
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
        for (offset, [r, g, b]) in [0.0, 0.58, 1.0]
            .into_iter()
            .zip(self.imp().band_stops.get())
        {
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
        let mut downloader = gtk::gdk::TextureDownloader::new(texture);
        downloader.set_format(gtk::gdk::MemoryFormat::R8g8b8a8);
        let (pixels, stride) = downloader.download_bytes();
        let stops = ::artwork::showcase_band(
            &pixels,
            texture.width().max(0) as u32,
            texture.height().max(0) as u32,
            stride as u32,
        );
        let imp = self.imp();
        if imp.band_stops.replace(stops) != stops {
            imp.band.queue_draw();
        }
    }

    pub fn set_title(&self, text: &str) {
        self.imp().title.set_text(text);
        self.refit();
    }

    /// Adds a line under the title: an album's artist, or the sub-page an artist's band heads.
    pub fn append_detail(&self, child: &impl IsA<gtk::Widget>) {
        self.imp().details.set_visible(true);
        self.imp().details.append(child);
    }

    /// Adds a fact after the summary's: an artist's album and track counts.
    pub fn append_fact(&self, child: &impl IsA<gtk::Widget>) {
        // Buttons (an artist's counts) carry their own boxes; the row around them takes none.
        self.imp().facts_fade.remove_css_class("detail-facts-boxed");
        self.imp().summary.append(child);
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

    /// Ends the facts row with `links`: the external sites, or the playlist's source.
    pub fn replace_external_links(&self, links: Option<gtk::Widget>) {
        let host = &self.imp().external_links;
        while let Some(child) = host.first_child() {
            host.remove(&child);
        }
        if let Some(links) = links {
            host.append(&links);
        }
    }

    /// Shows `genres` as chips that fill the width; those that do not fit go under +n.
    pub(crate) fn set_genres<'a>(
        &self,
        shell: &Rc<CatalogUi>,
        genres: impl IntoIterator<Item = (&'a str, library::GenreKey)>,
    ) {
        let imp = self.imp();
        imp.genres.set_width_callback(None);
        while let Some(child) = imp.genre_row.first_child() {
            if child == *imp.genre_overflow {
                break;
            }
            imp.genre_row.remove(&child);
        }
        while let Some(child) = imp.genre_overflow_list.first_child() {
            imp.genre_overflow_list.remove(&child);
        }
        let mut chips = Vec::new();
        for (name, key) in genres
            .into_iter()
            .map(|(name, key)| (name.trim(), key))
            .filter(|(name, _)| !name.is_empty())
        {
            let chip = genre_chip(name);
            let shell = Rc::downgrade(shell);
            chip.connect_clicked(move |_| {
                if let Some(shell) = shell.upgrade() {
                    shell.open_route(Route::GenreDetail(key));
                }
            });
            imp.genre_overflow_list.append(&chip);
            chips.push(chip);
        }
        imp.genres.set_visible(!chips.is_empty());
        let overflow = imp.genre_overflow.get();
        let row = imp.genre_row.get();
        let popup = imp.genre_overflow_list.get();
        imp.genres.set_width_callback(Some(Rc::new(move |width| {
            let widths: Vec<_> = chips
                .iter()
                .map(|chip| chip.measure(gtk::Orientation::Horizontal, -1).1 + GENRE_SPACING)
                .collect();
            let fits = |count: usize, reserve: i32| {
                widths[..count].iter().sum::<i32>() + reserve <= width + GENRE_SPACING
            };
            let count = if fits(chips.len(), 0) {
                chips.len()
            } else {
                overflow.set_label(&format!("+{}", chips.len()));
                let reserve = overflow.measure(gtk::Orientation::Horizontal, -1).1 + GENRE_SPACING;
                (0..chips.len())
                    .rev()
                    .find(|count| fits(*count, reserve))
                    .unwrap_or(0)
            };
            for (parent, shown) in [(&row, &chips[..count]), (&popup, &chips[count..])] {
                let mut previous = None;
                for chip in shown {
                    if chip.parent().as_ref() != Some(parent.upcast_ref()) {
                        if let Some(old) = chip.parent().and_downcast::<gtk::Box>() {
                            old.remove(chip);
                        }
                        parent.insert_child_after(chip, previous);
                    }
                    previous = Some(chip);
                }
            }
            overflow.set_label(&format!("+{}", chips.len() - count));
            overflow.set_visible(count < chips.len());
        })));
    }

    /// Mounts the header for `row`: its cover with the hover controls, its buttons, and its
    /// shape at each width.
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
            play_label,
            play,
            menu,
            download,
        } = mount;
        let (controls, cover_favorite) = cover_hover_controls_with_favorite(0, play_label, false);
        let favorite = self.imp().favorite.downgrade();
        cover_favorite.connect_clicked(move |_| {
            if let Some(favorite) = favorite.upgrade() {
                favorite.emit_clicked();
            }
        });
        self.imp().cover_favorite.replace(Some(cover_favorite));
        for (button, placement) in [
            (&controls.play, QueuePlacement::Now),
            (&controls.play_next, QueuePlacement::Next),
            (&controls.play_last, QueuePlacement::Last),
        ] {
            let play = Rc::clone(&play);
            button.connect_clicked(move |_| play(placement, false));
        }
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
        self.imp().cover_host.append(&showcase_cover_overlay(
            button.upcast_ref(),
            controls,
            Some(Rc::clone(&menu)),
        ));
        self.bind_actions(shell, play, menu, download);
        self.show_row(row);
        if shape == ShowcaseShape::Home {
            self.place_facts_on_band();
        }
        self.present(shape, initial_width, Some(cover))
    }

    /// Mounts a header with only its band: an artist's sub-page.
    pub fn mount_plain(&self, initial_width: i32) -> gtk::Widget {
        let imp = self.imp();
        imp.cover_host.set_visible(false);
        imp.below.set_visible(false);
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
        let cover_favorite = imp.cover_favorite.borrow().clone();
        if let Some(cover_favorite) = &cover_favorite {
            // Hover shows and hides it, so a row without a favorite keeps it unmapped.
            cover_favorite.set_child_visible(offered.favorite);
        }
        if let Some((_, favorite)) = row_favorite(&row) {
            for button in std::iter::once(&*imp.favorite).chain(cover_favorite.as_ref()) {
                set_favorite_button_active(button, favorite);
            }
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

    /// Beside the cover: the band tall enough that the title, its line and the facts end above its
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

    /// The row the header shows.
    pub fn row(&self) -> Option<Row> {
        self.imp().row.borrow().clone()
    }

    /// The page and the row the buttons act on.
    fn shown(&self) -> Option<(Rc<CatalogUi>, Row)> {
        let imp = self.imp();
        Some((imp.shell.borrow().upgrade()?, imp.row.borrow().clone()?))
    }

    /// Home's facts sit on the band, under the title, with nothing under the band.
    fn place_facts_on_band(&self) {
        let imp = self.imp();
        imp.below.remove(&*imp.facts);
        imp.names.append(&*imp.facts);
        imp.facts.set_margin_top(6);
        imp.facts_fade.add_css_class("detail-facts-pill");
        imp.below.set_visible(false);
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
        // Beside the cover, the title sits on the band, which is always dark; under the cover, the
        // title and its line are centred on it.
        if layout.stacked {
            imp.names.remove_css_class("detail-on-band");
        } else {
            imp.names.add_css_class("detail-on-band");
        }
        imp.title.set_xalign(if layout.stacked { 0.5 } else { 0.0 });
        // Under it the facts, links, genres and buttons are centred too.
        let along = if layout.stacked {
            gtk::Align::Center
        } else {
            gtk::Align::Start
        };
        imp.facts.set_halign(if layout.stacked {
            gtk::Align::Center
        } else {
            gtk::Align::Fill
        });
        imp.external_links.set_hexpand(!layout.stacked);
        imp.genre_row.set_halign(along);
        imp.actions.set_halign(along);
        imp.details.set_halign(if layout.stacked {
            gtk::Align::Center
        } else {
            gtk::Align::Fill
        });
        imp.body.set_orientation(if layout.stacked {
            gtk::Orientation::Vertical
        } else {
            gtk::Orientation::Horizontal
        });
        imp.cover_host.set_margin_top(layout.cover_top);
        imp.cover_host.set_halign(if layout.stacked {
            gtk::Align::Center
        } else {
            gtk::Align::Start
        });
        // Beside the cover the title ends just above the band's edge, and the rest starts
        // just under it.
        imp.heading
            .set_height_request(if layout.stacked { -1 } else { layout.band - 12 });
        imp.below
            .set_margin_top(if layout.stacked { 10 } else { 24 });
        imp.actions
            .set_margin_top(if imp.below.is_visible() { 18 } else { 24 });
        let step_top = (layout.band - STEP_BUTTON_SIZE) / 2;
        imp.step_previous.set_margin_top(step_top);
        imp.step_next.set_margin_top(step_top);
        // Room under a cover that sits beside the text, so its shadow is not cut.
        imp.body
            .set_margin_bottom(if layout.stacked || layout.cover == 0 {
                0
            } else {
                28
            });
        // Play, then Radio, give up their labels while the buttons do not fit their room, so the
        // row stays one row with ⋯ in view.
        let room = if layout.stacked {
            width - 2 * layout.inset
        } else {
            width - 2 * layout.inset - layout.cover - layout.gap
        };
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
        if !layout.stacked {
            self.fit_text(layout, width);
        }
    }
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
        let Some(window) = window.upgrade() else {
            return;
        };
        let launcher = gtk::UriLauncher::new(&url);
        gtk::glib::spawn_future_local(async move {
            if let Err(error) = launcher.launch_future(Some(&window)).await {
                warn!(%error, "failed to open external detail link");
            }
        });
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
