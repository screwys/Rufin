use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use ::library::{
    GenreSummary, HomeBlockKind, HomeSectionKind, HomeSnapshot, LoadedHomeSection, RadioSeed,
    ShowcaseItem,
};
use adw::prelude::*;
use gtk::{gio, glib};
use localization::{album_count_text, msgid, track_count_text};
use playback::RadioPlayRequest;

use crate::format_duration_units;
use crate::layout::{configure_fill_width_clip, width_allocation_owner};
use crate::localization::{bind_label_text_with, localized_label};
use crate::shell::Shell;
use crate::shell::cover::presentation::{add_album_seed_gradient_class, stable_seed};
use crate::shell::route::MountedRoute;

use super::cards::{album_cover_overlay, track_cover_overlay};
use super::collections::{home_item_row, library_route_inset};
use super::detail_links::{DetailLinkBinding, album_artist_links, track_artist_links};
use super::detail_showcase::{DetailSummaryProjection, detail_radio_button};
use super::grid_cells::collection_grid_column_count;
use super::home_layout::{
    HomeShowcaseMode, home_section_header, home_showcase_cover_size, home_showcase_is_compact,
    home_showcase_mode, home_showcase_spacing,
};
use super::library_fields::{COLLECTION_GRID_CARD_MARGIN, nonzero_year};
use super::route::Route;
use super::route_layout::{ROUTE_TOP_MARGIN, home_album_content_width, route_scroller_widget};

fn render_home_section_page_model(
    model: &gio::ListStore,
    section: &LoadedHomeSection,
    page_start: usize,
    page_size: usize,
) -> (usize, usize) {
    let item_count = section.items.len();
    let page_size = page_size.max(1);
    let page_start = clamped_home_section_page_start(page_start, page_size, item_count);
    let page_end = page_start.saturating_add(page_size).min(item_count);
    let additions = section.items[page_start..page_end]
        .iter()
        .cloned()
        .map(glib::BoxedAnyObject::new)
        .collect::<Vec<_>>();
    model.splice(0, model.n_items(), &additions);
    (page_start, page_end)
}

fn clamped_home_section_page_start(
    page_start: usize,
    page_size: usize,
    item_count: usize,
) -> usize {
    if item_count == 0 {
        return 0;
    }
    let page_size = page_size.max(1);
    let last_page_start = ((item_count - 1) / page_size) * page_size;
    page_start.min(last_page_start)
}

#[derive(Clone)]
struct MountedHomeSection {
    root: gtk::Box,
    presentation: MountedHomeSectionPresentation,
}

impl MountedHomeSection {
    fn replace(&self, section: Option<Arc<LoadedHomeSection>>) {
        self.root.set_visible(section.is_some());
        self.presentation.replace(section);
    }
}

#[derive(Clone)]
struct MountedHomeSectionPresentation {
    section: Rc<RefCell<Option<Arc<LoadedHomeSection>>>>,
    model: gio::ListStore,
    row: super::grid_cells::FixedPageCollectionRow,
    previous: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    page_start: Rc<Cell<usize>>,
    page_size: Rc<Cell<usize>>,
}

impl MountedHomeSectionPresentation {
    fn replace(&self, section: Option<Arc<LoadedHomeSection>>) {
        self.section.replace(section);
        self.render();
    }

    fn render(&self) {
        let section = self.section.borrow();
        let Some(section) = section.as_ref() else {
            self.model.remove_all();
            self.page_start.set(0);
            self.set_page_buttons(false, false);
            return;
        };
        let (page_start, page_end) = render_home_section_page_model(
            &self.model,
            section,
            self.page_start.get(),
            self.page_size.get(),
        );
        self.page_start.set(page_start);
        self.set_page_buttons(page_start > 0, page_end < section.items.len());
    }

    fn set_page_buttons(&self, previous: bool, next: bool) {
        if let Some(button) = self.previous.upgrade() {
            button.set_sensitive(previous);
        }
        if let Some(button) = self.next.upgrade() {
            button.set_sensitive(next);
        }
    }

    fn shift(&self, direction: HomeSectionPageDirection) {
        let Some(item_count) = self
            .section
            .borrow()
            .as_ref()
            .map(|section| section.items.len())
        else {
            return;
        };
        if item_count == 0 {
            return;
        }
        let page_size = self.page_size.get().max(1);
        match direction {
            HomeSectionPageDirection::Previous => self
                .page_start
                .set(self.page_start.get().saturating_sub(page_size)),
            HomeSectionPageDirection::Next => {
                let next = self.page_start.get().saturating_add(page_size);
                if next < item_count {
                    self.page_start.set(next);
                }
            }
        }
        self.render();
    }

    fn fit_width(&self, width: i32) {
        if width <= 1 {
            return;
        }
        let page_size = collection_grid_column_count(width);
        if self.page_size.replace(page_size) == page_size {
            return;
        }
        self.row.set_page_size(page_size);
        self.render();
    }

    fn reset_page(&self) {
        self.page_start.set(0);
    }
}

#[derive(Clone, Copy)]
enum HomeSectionPageDirection {
    Previous,
    Next,
}

#[derive(Clone)]
struct HomeRouteProjection {
    shell: Rc<Shell>,
    home: Rc<RefCell<Arc<HomeSnapshot>>>,
    section_views: Rc<RefCell<HashMap<HomeSectionKind, MountedHomeSection>>>,
    section_slots: Rc<HashMap<HomeSectionKind, gtk::Box>>,
    content: gtk::Box,
    block_roots: Rc<HashMap<HomeBlockKind, gtk::Widget>>,
    showcase_slot: gtk::Box,
    genres_slot: gtk::Box,
    empty: gtk::Widget,
    applied_blocks: Rc<RefCell<Vec<HomeBlockKind>>>,
}

impl HomeRouteProjection {
    fn build(&self) {
        let home = Arc::clone(&self.home.borrow());
        if let Some(showcase) = home.showcase.as_ref() {
            self.showcase_slot
                .append(&self.shell.home_showcase_block(showcase));
        }
        if let Some(genres) = self.shell.home_genres_block(&home.genres) {
            self.genres_slot.append(&genres);
        }
        for section in home.sections.iter() {
            self.install_section(Arc::clone(section));
        }
        self.apply_block_settings();
    }

    fn install_section(&self, section: Arc<LoadedHomeSection>) {
        let kind = section.kind;
        let Some(slot) = self.section_slots.get(&kind) else {
            return;
        };
        let mut views = self.section_views.borrow_mut();
        if let Some(view) = views.get(&kind) {
            view.replace(Some(section));
            return;
        }
        let view = self.shell.mounted_home_section(section);
        slot.append(&view.root);
        views.insert(kind, view);
    }

    fn replace_section(&self, kind: HomeSectionKind, home: Arc<HomeSnapshot>) {
        let section = home.section(kind).cloned();
        self.home.replace(home);
        match section {
            Some(section) => self.install_section(section),
            None => {
                if let Some(view) = self.section_views.borrow_mut().remove(&kind)
                    && let Some(slot) = self.section_slots.get(&kind)
                {
                    view.replace(None);
                    slot.remove(&view.root);
                }
            }
        }
        self.apply_block_settings();
    }

    fn apply_block_settings(&self) {
        let blocks = self.shell.settings.current.borrow().home_blocks.clone();
        let mut previous = None::<gtk::Widget>;
        for block in &blocks {
            let Some(root) = self.block_roots.get(block) else {
                continue;
            };
            self.content.reorder_child_after(root, previous.as_ref());
            previous = Some(root.clone());
        }
        for (block, root) in self.block_roots.iter() {
            root.set_visible(blocks.contains(block) && self.block_available(*block));
        }
        self.content
            .reorder_child_after(&self.empty, previous.as_ref());
        self.empty.set_visible(
            !blocks
                .iter()
                .filter_map(|block| self.block_roots.get(block))
                .any(gtk::Widget::is_visible),
        );
        self.applied_blocks.replace(blocks);
    }

    fn reconcile_block_settings(&self) {
        let blocks = self.shell.settings.current.borrow().home_blocks.clone();
        if *self.applied_blocks.borrow() != blocks {
            self.apply_block_settings();
        }
    }

    fn block_available(&self, block: HomeBlockKind) -> bool {
        match block {
            HomeBlockKind::Showcase => self.showcase_slot.first_child().is_some(),
            HomeBlockKind::Genres => self.genres_slot.first_child().is_some(),
            _ => block
                .section_kind()
                .is_some_and(|kind| self.home.borrow().section(kind).is_some()),
        }
    }
}

impl Shell {
    pub(crate) fn home_route(self: &Rc<Self>, home: Arc<HomeSnapshot>) -> MountedRoute {
        let scroller = gtk::ScrolledWindow::new();
        configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("route-content");
        content.set_hexpand(true);
        content.set_halign(gtk::Align::Fill);
        content.set_width_request(1);
        content.set_margin_top(ROUTE_TOP_MARGIN);
        content.set_margin_bottom(36);

        let mut section_slots = HashMap::new();
        let mut block_roots = HashMap::new();
        let mut showcase_slot = None;
        let mut genres_slot = None;
        for block in HomeBlockKind::all() {
            let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
            slot.set_hexpand(true);
            content.append(&slot);
            block_roots.insert(block, slot.clone().upcast());
            match block {
                HomeBlockKind::Showcase => showcase_slot = Some(slot),
                HomeBlockKind::Genres => genres_slot = Some(slot),
                _ => {
                    if let Some(kind) = block.section_kind() {
                        section_slots.insert(kind, slot);
                    }
                }
            }
        }
        let empty = self.route_empty_view(msgid("Nothing here yet"));
        content.append(&empty);
        let inset_content = library_route_inset(content.clone().upcast());
        inset_content.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        scroller.set_child(Some(&inset_content));

        let projection = HomeRouteProjection {
            shell: Rc::clone(self),
            home: Rc::new(RefCell::new(home)),
            section_views: Rc::new(RefCell::new(HashMap::new())),
            section_slots: Rc::new(section_slots),
            content,
            block_roots: Rc::new(block_roots),
            showcase_slot: showcase_slot.expect("Home showcase slot"),
            genres_slot: genres_slot.expect("Home genres slot"),
            empty,
            applied_blocks: Rc::new(RefCell::new(Vec::new())),
        };
        projection.build();
        let widget = route_scroller_widget(scroller);
        let resume_projection = projection.clone();
        let resume = Rc::new(move || resume_projection.reconcile_block_settings());
        let apply_projection = projection.clone();
        let apply_home_section = Rc::new(move |kind, next: Arc<HomeSnapshot>| {
            let current = Arc::clone(&apply_projection.home.borrow());
            let merged = Arc::new(current.replacing_section(kind, next.section(kind).cloned()));
            apply_projection.replace_section(kind, merged);
        });
        MountedRoute::new(widget, resume).with_home_section_applier(apply_home_section)
    }

    fn home_showcase_block(self: &Rc<Self>, item: &ShowcaseItem) -> gtk::Widget {
        let width = home_album_content_width(self);
        let mode = home_showcase_mode(width);
        let cover_size = home_showcase_cover_size(width);
        let (
            seed,
            cover,
            title_text,
            artist_text,
            year,
            track_count,
            duration,
            artist_links,
            radio,
        ) = match item {
            ShowcaseItem::Album(album) => (
                album.album.color_seed,
                album_cover_overlay(self, album, cover_size),
                album.album.title.clone(),
                album.album.artist.clone(),
                album.album.year,
                album.track_count,
                album.duration_seconds,
                album_artist_links(&album.album),
                RadioSeed::Album(album.album.id.clone()),
            ),
            ShowcaseItem::Track(track) => (
                stable_seed(track.id.as_str()),
                track_cover_overlay(self, track.clone(), cover_size),
                track.title.clone(),
                track.artist.clone(),
                track.year,
                1,
                track.duration_seconds,
                track_artist_links(track),
                RadioSeed::Track(track.id.clone()),
            ),
        };

        let section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        section.set_hexpand(true);
        let body = gtk::Box::new(gtk::Orientation::Horizontal, home_showcase_spacing(width));
        body.add_css_class("home-showcase");
        add_album_seed_gradient_class(&body, seed);
        body.set_hexpand(true);
        body.set_halign(gtk::Align::Fill);
        body.set_valign(gtk::Align::Start);
        body.set_width_request(1);
        body.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        body.set_overflow(gtk::Overflow::Hidden);
        cover.widget().add_css_class("home-showcase-cover");
        let cover_column = gtk::Box::new(gtk::Orientation::Vertical, 8);
        cover_column.set_width_request(cover_size);
        cover_column.set_halign(gtk::Align::Start);
        cover_column.append(&cover.widget());
        body.append(&cover_column);

        let facts = DetailSummaryProjection::new(&[
            ("rufin-x-office-calendar-symbolic", nonzero_year(year)),
            (
                "rufin-tracks-symbolic",
                track_count_text(track_count.into()),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units(duration),
            ),
        ]);
        facts.bind_text_with(1, move || track_count_text(track_count.into()));
        let metadata = gtk::Box::new(gtk::Orientation::Vertical, 10);
        metadata.set_hexpand(true);
        metadata.set_halign(gtk::Align::Fill);
        metadata.set_valign(gtk::Align::Center);
        metadata.set_width_request(1);
        metadata.set_visible(mode != HomeShowcaseMode::CoverOnly);
        metadata.append(&self.home_showcase_kind_row(radio));

        let title = gtk::Label::new(Some(&title_text));
        title.add_css_class("home-showcase-title");
        if home_showcase_is_compact(width) {
            title.add_css_class("home-showcase-title-compact");
        }
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        title.set_lines(3);
        title.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.set_width_chars(1);
        metadata.append(&title);

        let artist = gtk::Label::new(Some(&artist_text));
        artist.add_css_class("muted");
        artist.set_xalign(0.0);
        artist.set_ellipsize(gtk::pango::EllipsizeMode::End);
        artist.set_width_chars(1);
        DetailLinkBinding::new(&artist, self).bind(artist_links);
        metadata.append(&artist);
        metadata.append(&facts.widget());
        body.append(&metadata);
        section.append(&body);

        let allocated_width = Rc::new(Cell::new(width));
        let resize_body = body;
        let resize_metadata = metadata;
        let resize_title = title;
        let resize_cover_column = cover_column;
        let resize_cover = cover;
        width_allocation_owner(&section, move |width| {
            if width <= 1 || allocated_width.replace(width) == width {
                return;
            }
            let mode = home_showcase_mode(width);
            resize_body.set_spacing(home_showcase_spacing(width));
            resize_metadata.set_visible(mode != HomeShowcaseMode::CoverOnly);
            if home_showcase_is_compact(width) {
                resize_title.add_css_class("home-showcase-title-compact");
            } else {
                resize_title.remove_css_class("home-showcase-title-compact");
            }
            let cover_size = home_showcase_cover_size(width);
            resize_cover_column.set_width_request(cover_size);
            resize_cover.resize(cover_size);
        })
        .upcast()
    }

    fn home_showcase_kind_row(self: &Rc<Self>, seed: RadioSeed) -> gtk::Box {
        let label = localized_label("Showcase");
        label.add_css_class("eyebrow");
        label.set_xalign(0.0);
        label.set_halign(gtk::Align::Start);
        label.set_valign(gtk::Align::Center);
        label.set_margin_end(6);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        row.add_css_class("album-detail-kind-row");
        row.add_css_class("album-detail-genre-row");
        row.add_css_class("home-showcase-kind-row");
        row.set_valign(gtk::Align::Center);
        row.set_halign(gtk::Align::Start);
        row.append(&label);
        let radio = detail_radio_button();
        let controller = self.products.playback.radio.clone();
        radio.connect_clicked(move |_| {
            controller.play_radio(RadioPlayRequest::now(seed.clone()));
        });
        row.append(&radio);
        row
    }

    fn home_genres_block(self: &Rc<Self>, genres: &[GenreSummary]) -> Option<gtk::Widget> {
        if genres.is_empty() {
            return None;
        }
        let section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        section.set_hexpand(true);
        let heading = localized_label(HomeBlockKind::Genres.title());
        heading.add_css_class("section-heading");
        heading.set_xalign(0.0);
        heading.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        section.append(&heading);
        let flow = gtk::FlowBox::new();
        flow.add_css_class("home-genre-flow");
        flow.set_column_spacing(8);
        flow.set_row_spacing(8);
        flow.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_max_children_per_line(6);
        flow.set_min_children_per_line(2);
        for genre in genres {
            flow.insert(&self.home_genre_chip(genre), -1);
        }
        section.append(&flow);
        Some(section.upcast())
    }

    fn home_genre_chip(self: &Rc<Self>, genre: &GenreSummary) -> gtk::Widget {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("home-genre-chip");
        button.set_hexpand(true);
        button.set_halign(gtk::Align::Fill);
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
        labels.set_margin_top(8);
        labels.set_margin_bottom(8);
        labels.set_margin_start(10);
        labels.set_margin_end(10);
        let name = gtk::Label::new(Some(&genre.genre.name));
        name.add_css_class("album-title");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&name);
        let counts = gtk::Label::new(None);
        let album_count = u64::from(genre.album_count);
        let track_count = u64::from(genre.track_count);
        bind_label_text_with(&counts, move || {
            format!(
                "{} • {}",
                album_count_text(album_count),
                track_count_text(track_count)
            )
        });
        counts.add_css_class("muted");
        counts.set_xalign(0.0);
        counts.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&counts);
        button.set_child(Some(&labels));
        let shell = Rc::clone(self);
        let genre_id = genre.genre.id.clone();
        button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(genre_id.clone())));
        button.upcast()
    }

    fn mounted_home_section(
        self: &Rc<Self>,
        section_data: Arc<LoadedHomeSection>,
    ) -> MountedHomeSection {
        let section_kind = section_data.kind;
        let section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        section.set_hexpand(true);
        let header = home_section_header(section_kind.title());
        header.root.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        let previous = header.previous.clone();
        let next = header.next.clone();
        let refresh = header.refresh.clone();
        section.append(&header.root);

        let content_width = home_album_content_width(self);
        let page_size = collection_grid_column_count(content_width);
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let row = home_item_row(self, model.clone(), page_size);
        let presentation = MountedHomeSectionPresentation {
            section: Rc::new(RefCell::new(None)),
            model,
            row: row.clone(),
            previous: previous.downgrade(),
            next: next.downgrade(),
            page_start: Rc::new(Cell::new(0)),
            page_size: Rc::new(Cell::new(page_size)),
        };
        let fit_presentation = presentation.clone();
        let width_owner = width_allocation_owner(&row.widget(), move |width| {
            fit_presentation.fit_width(width);
        });
        section.append(&width_owner);
        let view = MountedHomeSection {
            root: section,
            presentation: presentation.clone(),
        };
        view.replace(Some(section_data));

        let previous_view = presentation.clone();
        previous.connect_clicked(move |_| {
            previous_view.shift(HomeSectionPageDirection::Previous);
        });
        let next_view = presentation.clone();
        next.connect_clicked(move |_| {
            next_view.shift(HomeSectionPageDirection::Next);
        });
        let refresh_view = presentation;
        let source = self.selected_source_operations();
        refresh.connect_clicked(move |_| {
            refresh_view.reset_page();
            if let Some(source) = source.as_ref() {
                if home_section_refreshes_library(section_kind) {
                    source.refresh_library();
                } else {
                    source.refresh_home(section_kind);
                }
            }
        });
        view
    }
}

fn home_section_refreshes_library(kind: HomeSectionKind) -> bool {
    kind == HomeSectionKind::NewlyAdded
}

#[cfg(test)]
mod tests {
    use library::HomeSectionKind;

    use super::{clamped_home_section_page_start, home_section_refreshes_library};

    #[test]
    fn home_page_start_stays_on_a_complete_available_page() {
        assert_eq!(clamped_home_section_page_start(12, 6, 14), 12);
        assert_eq!(clamped_home_section_page_start(18, 6, 14), 12);
        assert_eq!(clamped_home_section_page_start(10, 6, 0), 0);
    }

    #[test]
    fn newly_added_refreshes_authoritative_library_facts() {
        assert!(home_section_refreshes_library(HomeSectionKind::NewlyAdded));
        for kind in [
            HomeSectionKind::Explore,
            HomeSectionKind::MostPlayed,
            HomeSectionKind::RecentlyPlayed,
            HomeSectionKind::RecentlyReleased,
        ] {
            assert!(!home_section_refreshes_library(kind));
        }
    }
}
