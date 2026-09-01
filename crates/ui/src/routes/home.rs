use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use library::{HomeAlbumRow, HomeGenreRow, HomePage, HomeSectionRows, HomeTrackRow, RadioSeed};
use localization::{album_count_text, msgid, tr, track_count_text};
use playback::RadioPlayRequest;

use crate::LibraryField;
use crate::favorites::{
    favorite_button_is_active, favorite_icon_button, set_favorite_button_active,
};
use crate::format_duration_units;
use crate::layout::{configure_fill_width_clip, width_allocation_owner};
use crate::localization::{bind_label_text_with, localized_label};
use crate::settings::{HomeBlockKind, HomeSectionKind};
use crate::shell::Shell;
use crate::shell::actions::{ActionButtonVariant, configure_action_button};
use crate::shell::cover::presentation::stable_seed;
use crate::shell::route::MountedRoute;

use super::collection_context::present_album_context_menu;
use super::collections::{CollectionPlay, PlaybackTarget, library_route_inset};
use super::detail_links::{
    DetailLinkBinding, DetailLinks, album_artist_links, track_album_artist_links,
    track_artist_links,
};
use super::detail_showcase::{
    DetailShowcaseView, MediaShowcase, detail_playback_controls, detail_radio_button,
    home_album_cover_projection, media_showcase,
};
use super::grid_cells::{
    AlbumGridCell, FixedPageCollectionRow, ReusableCollectionGridCell, TrackGridCell,
    collection_grid_column_count, fixed_page_collection_row,
};
use super::home_layout::home_section_header;
use super::library_fields::{COLLECTION_GRID_CARD_MARGIN, track_field};
use super::playlist_picker::{PlaylistTrackSource, install_compact_playlist_drag_source};
use super::route::Route;
use super::route_layout::{ROUTE_TOP_MARGIN, home_album_content_width, route_scroller_widget};

const HOME_ALBUM_GRID_FIELDS: [LibraryField; 2] = [LibraryField::AlbumArtist, LibraryField::Year];
const HOME_TRACK_GRID_FIELDS: [LibraryField; 2] = [LibraryField::Artist, LibraryField::Album];
const HOME_SHOWCASE_ACTION_MIN_COVER_SIZE: i32 = 200;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HomeRefreshAction {
    Library,
    Section(HomeSectionKind),
}

fn home_refresh_action(kind: HomeSectionKind) -> HomeRefreshAction {
    if kind == HomeSectionKind::NewlyAdded {
        HomeRefreshAction::Library
    } else {
        HomeRefreshAction::Section(kind)
    }
}

fn next_home_variation(current: i64) -> i64 {
    current.wrapping_add(1)
}

#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "Home sections are hard-bounded and keep final rows inline"
)]
enum HomeItem {
    Track(HomeTrackRow),
    Album(HomeAlbumRow),
}

#[derive(Clone)]
struct MountedHomeSection {
    data: Rc<RefCell<Vec<HomeItem>>>,
    model: gio::ListStore,
    row: FixedPageCollectionRow,
    previous: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    page_start: Rc<Cell<usize>>,
    page_size: Rc<Cell<usize>>,
}

impl MountedHomeSection {
    fn replace(&self, items: Vec<HomeItem>) {
        self.data.replace(items);
        self.render();
    }

    fn render(&self) {
        let data = self.data.borrow();
        let page_size = self.page_size.get().max(1);
        let page_start = clamped_page_start(self.page_start.get(), page_size, data.len());
        let page_end = page_start.saturating_add(page_size).min(data.len());
        let additions = data[page_start..page_end]
            .iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect::<Vec<_>>();
        self.model.splice(0, self.model.n_items(), &additions);
        self.page_start.set(page_start);
        if let Some(previous) = self.previous.upgrade() {
            previous.set_sensitive(page_start > 0);
        }
        if let Some(next) = self.next.upgrade() {
            next.set_sensitive(page_end < data.len());
        }
    }

    fn shift(&self, next: bool) {
        let page_size = self.page_size.get().max(1);
        if next {
            if self.page_start.get().saturating_add(page_size) < self.data.borrow().len() {
                self.page_start
                    .set(self.page_start.get().saturating_add(page_size));
            }
        } else {
            self.page_start
                .set(self.page_start.get().saturating_sub(page_size));
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
}

#[derive(Clone)]
struct HomeSectionView {
    root: gtk::Widget,
    mounted: MountedHomeSection,
}

#[derive(Clone)]
struct HomeRouteProjection {
    shell: std::rc::Weak<Shell>,
    home: Rc<RefCell<HomePage>>,
    slots: Rc<std::collections::HashMap<HomeBlockKind, gtk::Box>>,
    sections: Rc<RefCell<std::collections::HashMap<HomeSectionKind, HomeSectionView>>>,
    provider_sections: Rc<RefCell<std::collections::HashMap<String, HomeSectionView>>>,
    content: gtk::Box,
    provider_slot: gtk::Box,
    empty: gtk::Widget,
    applied_blocks: Rc<RefCell<Vec<HomeBlockKind>>>,
}

impl HomeRouteProjection {
    fn apply(&self, home: HomePage) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let previous = self.home.replace(home.clone());
        if previous.showcase != home.showcase
            && let Some(slot) = self.slots.get(&HomeBlockKind::Showcase)
        {
            while let Some(child) = slot.first_child() {
                slot.remove(&child);
            }
            if let Some(showcase) = home.showcase.as_ref() {
                slot.append(&shell.home_showcase(showcase));
            }
        }
        for (block, kind, rows) in [
            (
                HomeBlockKind::Explore,
                HomeSectionKind::Explore,
                HomeSectionRows {
                    tracks: home.explore.clone(),
                    albums: Vec::new(),
                },
            ),
            (
                HomeBlockKind::MostPlayed,
                HomeSectionKind::MostPlayed,
                home.most_played.clone(),
            ),
            (
                HomeBlockKind::NewlyAdded,
                HomeSectionKind::NewlyAdded,
                home.newly_added.clone(),
            ),
            (
                HomeBlockKind::RecentlyPlayed,
                HomeSectionKind::RecentlyPlayed,
                home.recently_played.clone(),
            ),
            (
                HomeBlockKind::RecentlyReleased,
                HomeSectionKind::RecentlyReleased,
                home.recently_released.clone(),
            ),
        ] {
            let items = home_items(&rows);
            if let Some(view) = self.sections.borrow().get(&kind).cloned() {
                view.root.set_visible(!items.is_empty());
                view.mounted.replace(items);
            } else if !items.is_empty()
                && let Some(slot) = self.slots.get(&block)
            {
                let view = shell.home_section_view(&tr(block.title()), items, Some(kind));
                slot.append(&view.root);
                self.sections.borrow_mut().insert(kind, view);
            }
        }
        if previous.genres != home.genres
            && let Some(slot) = self.slots.get(&HomeBlockKind::Genres)
        {
            while let Some(child) = slot.first_child() {
                slot.remove(&child);
            }
            if let Some(genres) = shell.home_genres(&home.genres) {
                slot.append(&genres);
            }
        }
        let mut live_provider_ids = Vec::new();
        let mut previous_provider = None::<gtk::Widget>;
        for provider in &home.provider_sections {
            live_provider_ids.push(provider.section_id.clone());
            let title = provider
                .section_id
                .split(['-', '_'])
                .filter(|part| !part.is_empty())
                .map(title_case)
                .collect::<Vec<_>>()
                .join(" ");
            let items = home_items(&provider.rows);
            if !items.is_empty() {
                let view = self
                    .provider_sections
                    .borrow()
                    .get(&provider.section_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        let view = shell.home_section_view(&title, Vec::new(), None);
                        self.provider_slot.append(&view.root);
                        self.provider_sections
                            .borrow_mut()
                            .insert(provider.section_id.clone(), view.clone());
                        view
                    });
                view.root.set_visible(true);
                view.mounted.replace(items);
                self.provider_slot
                    .reorder_child_after(&view.root, previous_provider.as_ref());
                previous_provider = Some(view.root.clone());
            } else if let Some(view) = self
                .provider_sections
                .borrow()
                .get(&provider.section_id)
                .cloned()
            {
                view.mounted.replace(Vec::new());
                view.root.set_visible(false);
            }
        }
        let stale = self
            .provider_sections
            .borrow()
            .keys()
            .filter(|id| !live_provider_ids.contains(id))
            .cloned()
            .collect::<Vec<_>>();
        for id in stale {
            if let Some(view) = self.provider_sections.borrow_mut().remove(&id) {
                self.provider_slot.remove(&view.root);
            }
        }
        self.empty.set_visible(
            self.slots.values().all(|slot| !slot_has_content(slot))
                && !slot_has_content(&self.provider_slot),
        );
        self.apply_block_settings();
    }

    fn reconcile_block_settings(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let blocks = shell.settings.current.borrow().home_blocks.clone();
        if *self.applied_blocks.borrow() != blocks {
            self.apply_block_settings();
        }
    }

    fn apply_block_settings(&self) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let blocks = shell.settings.current.borrow().home_blocks.clone();
        let mut previous = None::<gtk::Widget>;
        for block in &blocks {
            let Some(slot) = self.slots.get(block) else {
                continue;
            };
            slot.set_visible(slot_has_content(slot));
            self.content.reorder_child_after(slot, previous.as_ref());
            previous = Some(slot.clone().upcast());
        }
        for block in HomeBlockKind::all() {
            if !blocks.contains(&block)
                && let Some(slot) = self.slots.get(&block)
            {
                slot.set_visible(false);
            }
        }
        self.content
            .reorder_child_after(&self.provider_slot, previous.as_ref());
        self.provider_slot
            .set_visible(slot_has_content(&self.provider_slot));
        self.content
            .reorder_child_after(&self.empty, Some(&self.provider_slot));
        self.empty.set_visible(
            blocks.iter().all(|block| {
                self.slots
                    .get(block)
                    .is_none_or(|slot| !slot_has_content(slot))
            }) && !slot_has_content(&self.provider_slot),
        );
        self.applied_blocks.replace(blocks);
    }
}

fn slot_has_content(slot: &gtk::Box) -> bool {
    let mut child = slot.first_child();
    visible_section_exists(std::iter::from_fn(move || {
        let current = child.take()?;
        child = current.next_sibling();
        Some(current.is_visible())
    }))
}

fn visible_section_exists(visibility: impl IntoIterator<Item = bool>) -> bool {
    visibility.into_iter().any(|visible| visible)
}

fn home_items(rows: &HomeSectionRows) -> Vec<HomeItem> {
    rows.tracks
        .iter()
        .cloned()
        .map(HomeItem::Track)
        .chain(rows.albums.iter().cloned().map(HomeItem::Album))
        .collect()
}

fn clamped_page_start(start: usize, page_size: usize, count: usize) -> usize {
    if count == 0 {
        0
    } else {
        start.min(((count - 1) / page_size.max(1)) * page_size.max(1))
    }
}

impl Shell {
    pub(crate) fn home_route(
        self: &Rc<Self>,
        _selected: crate::runtime::SelectedLibrary,
        home: HomePage,
    ) -> MountedRoute {
        let scroller = gtk::ScrolledWindow::new();
        configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("route-content");
        content.set_hexpand(true);
        content.set_halign(gtk::Align::Fill);
        content.set_width_request(1);
        content.set_margin_top(ROUTE_TOP_MARGIN);

        let mut slots = std::collections::HashMap::new();
        for block in HomeBlockKind::all() {
            let slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
            slot.set_hexpand(true);
            content.append(&slot);
            slots.insert(block, slot);
        }
        let provider_slot = gtk::Box::new(gtk::Orientation::Vertical, 18);
        provider_slot.set_hexpand(true);
        content.append(&provider_slot);
        let empty = self.route_empty_view(msgid("Nothing here yet"));
        content.append(&empty);
        let inset_content = library_route_inset(content.clone().upcast());
        inset_content.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        scroller.set_child(Some(&inset_content));

        let projection = HomeRouteProjection {
            shell: Rc::downgrade(self),
            home: Rc::new(RefCell::new(HomePage::default())),
            slots: Rc::new(slots),
            sections: Rc::new(RefCell::new(std::collections::HashMap::new())),
            provider_sections: Rc::new(RefCell::new(std::collections::HashMap::new())),
            content,
            provider_slot,
            empty,
            applied_blocks: Rc::new(RefCell::new(Vec::new())),
        };
        projection.apply(home);
        let widget = route_scroller_widget(scroller);
        let apply = projection.clone();
        let resume = projection.clone();
        MountedRoute::new(widget, Rc::new(move || resume.reconcile_block_settings()))
            .with_home_page_apply(Rc::new(move |home| apply.apply(home)))
    }

    fn home_section_view(
        self: &Rc<Self>,
        title: &str,
        items: Vec<HomeItem>,
        refresh_kind: Option<HomeSectionKind>,
    ) -> HomeSectionView {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        section.set_hexpand(true);
        let header = home_section_header(title);
        header.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        section.append(&header);

        let content_width = home_album_content_width(self);
        let page_size = collection_grid_column_count(content_width);
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let widget_shell = Rc::clone(self);
        let activate_shell = Rc::clone(self);
        let row = fixed_page_collection_row(
            model.clone(),
            page_size,
            move |_, item: HomeItem| home_item_widget(&widget_shell, item),
            move |_, item: HomeItem| activate_home_item(&activate_shell, item),
        );
        let mounted = MountedHomeSection {
            data: Rc::new(RefCell::new(items)),
            model,
            row: row.clone(),
            previous: header.previous().downgrade(),
            next: header.next().downgrade(),
            page_start: Rc::new(Cell::new(0)),
            page_size: Rc::new(Cell::new(page_size)),
        };
        mounted.render();
        let fit = mounted.clone();
        section.append(&width_allocation_owner(&row.widget(), move |width| {
            fit.fit_width(width);
        }));
        let previous = mounted.clone();
        header
            .previous()
            .connect_clicked(move |_| previous.shift(false));
        let next = mounted.clone();
        header.next().connect_clicked(move |_| next.shift(true));
        if let Some(kind) = refresh_kind {
            let shell = Rc::clone(self);
            let operations = self.selected_source_operations();
            let refresh_section = mounted.clone();
            header.refresh().connect_clicked(move |_| {
                refresh_section.page_start.set(0);
                if kind == HomeSectionKind::Explore {
                    shell
                        .home_explore_variation
                        .set(next_home_variation(shell.home_explore_variation.get()));
                }
                if let Some(operations) = operations.as_ref() {
                    match home_refresh_action(kind) {
                        HomeRefreshAction::Library => operations
                            .refresh_library(crate::runtime::LibraryRefreshTrigger::NewlyAdded),
                        HomeRefreshAction::Section(kind) => operations.refresh_home(kind),
                    }
                }
            });
        } else {
            header.refresh().set_visible(false);
        }
        HomeSectionView {
            root: section.upcast(),
            mounted,
        }
    }

    fn home_showcase(self: &Rc<Self>, item: &HomeAlbumRow) -> gtk::Widget {
        let width = home_album_content_width(self);
        let mut album = item.album.clone();
        album.title.clone_from(&item.title);
        if item.artwork_binding.is_some() {
            album.artwork_binding.clone_from(&item.artwork_binding);
        }
        let seed = stable_seed(&album.object_id);
        let cover = home_album_cover_projection(
            self,
            &album,
            super::route_layout::detail_showcase_cover_size(width),
        );
        let title_text = album.title.clone();
        let showcase_view =
            DetailShowcaseView::new("home-showcase", seed, "Showcase", true, &title_text);
        let radio = detail_radio_button();
        let radio_controller = self.products.playback.radio.clone();
        let radio_seed = RadioSeed::Album(album.album_key);
        radio.connect_clicked(move |_| {
            radio_controller.play_radio(RadioPlayRequest::now(radio_seed.clone()));
        });
        showcase_view.append_kind_control(&radio);
        let drag_album = album.album_key;
        let drag_title = title_text.clone();
        let drag_target: gtk::Widget = cover.button().upcast();
        let drag_artwork = cover.drag_paintable_source();
        let drag_shell = Rc::downgrade(self);
        install_compact_playlist_drag_source(&drag_target, &drag_artwork, move || {
            let shell = drag_shell.upgrade()?;
            let source =
                PlaylistTrackSource::current_target(&shell, PlaybackTarget::Album(drag_album))?;
            Some((source, drag_title.clone()))
        });

        let actions = showcase_view.actions();
        actions.set_halign(gtk::Align::Start);
        let play_shell = Rc::clone(self);
        let play_target = PlaybackTarget::Album(album.album_key);
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            play_target.play(&play_shell, placement, shuffled);
        });
        let controls = detail_playback_controls(
            &actions,
            msgid("Play album"),
            Some(album.favorite),
            true,
            play,
        );
        let favorite = favorite_icon_button("Favorite");
        configure_action_button(&favorite, ActionButtonVariant::DetailFavorite);
        set_favorite_button_active(&favorite, album.favorite);
        actions.append(&favorite);
        let hover_favorite = controls
            .favorite
            .as_ref()
            .expect("Home album has a Favorite cover control")
            .clone();
        let favorite_key = album.album_key;
        for button in [favorite, hover_favorite] {
            self.register_dynamic_favorite_button(
                Rc::new(move || Some(crate::favorites::album_favorite_key(&favorite_key))),
                &button,
            );
            let favorite_shell = Rc::clone(self);
            let favorite_album = album.album_key;
            button.connect_clicked(move |button| {
                favorite_shell.set_favorite_with_feedback(
                    library::FavoriteTarget::Album(favorite_album),
                    !favorite_button_is_active(button),
                    Some(button),
                );
            });
        }

        let menu_shell = Rc::clone(self);
        let menu_album = album.clone();
        let context_menu = Rc::new(move |target: &gtk::Widget, position| {
            present_album_context_menu(
                target,
                &menu_shell,
                menu_album.clone(),
                None,
                None,
                position,
            );
        });

        let artist = gtk::Label::new(Some(&album.display_artist));
        artist.add_css_class("detail-artist");
        artist.set_xalign(0.0);
        artist.set_halign(gtk::Align::Start);
        artist.set_wrap(true);
        artist.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        artist.set_width_request(1);
        artist.set_width_chars(1);
        artist.set_max_width_chars(32);
        DetailLinkBinding::new(&artist, self).bind(album_artist_links(&album));
        showcase_view.append_detail(&artist);
        showcase_view.replace_summary(&[
            (
                "rufin-x-office-calendar-symbolic",
                album.year.map(|year| year.to_string()).unwrap_or_default(),
            ),
            (
                "rufin-tracks-symbolic",
                track_count_text(album.track_count.max(0) as u64),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units((album.duration_millis.max(0) / 1_000) as u32),
            ),
        ]);
        let track_count = album.track_count.max(0) as u64;
        showcase_view.bind_summary_text_with(1, move || track_count_text(track_count));

        media_showcase(MediaShowcase {
            view: showcase_view,
            initial_width: width,
            cover,
            cover_controls: controls,
            context_menu: Some(context_menu),
            actions_min_cover_size: Some(HOME_SHOWCASE_ACTION_MIN_COVER_SIZE),
        })
    }

    fn home_genres(self: &Rc<Self>, genres: &[HomeGenreRow]) -> Option<gtk::Widget> {
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
        flow.set_selection_mode(gtk::SelectionMode::None);
        flow.set_max_children_per_line(6);
        flow.set_min_children_per_line(2);
        flow.set_margin_start(COLLECTION_GRID_CARD_MARGIN);
        for genre in genres {
            flow.insert(&self.home_genre_chip(genre), -1);
        }
        section.append(&flow);
        Some(section.upcast())
    }

    fn home_genre_chip(self: &Rc<Self>, genre: &HomeGenreRow) -> gtk::Widget {
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
        let name = gtk::Label::new(Some(&genre.name));
        name.add_css_class("album-title");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        labels.append(&name);
        let counts = gtk::Label::new(None);
        let album_count = genre.album_count.max(0) as u64;
        let track_count = genre.track_count.max(0) as u64;
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
        let key = genre.genre_key;
        button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(key)));
        button.upcast()
    }
}

fn title_case(part: &str) -> String {
    let mut chars = part.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default()
}

fn home_item_widget(shell: &Rc<Shell>, item: HomeItem) -> gtk::Widget {
    match item {
        HomeItem::Track(item) => {
            let mut track = item.track;
            track.title = item.title;
            if item.artwork_binding.is_some() {
                track.artwork_binding = item.artwork_binding;
            }
            let play_shell = Rc::clone(shell);
            let play_key = track.track_key;
            let play = Rc::new(move |_| {
                super::collections::PlaybackTarget::Track(play_key).play(
                    &play_shell,
                    playback::QueuePlacement::Now,
                    false,
                );
            });
            let field = Rc::new(move |_, track: &library::TrackRow, field| match field {
                LibraryField::Artist => track_artist_links(track),
                LibraryField::AlbumArtist => track_album_artist_links(track),
                LibraryField::Album => DetailLinks::route(
                    &track.display_album,
                    track.album_key.map(Route::AlbumDetail),
                ),
                _ => DetailLinks::text(&track_field(track, field)),
            });
            let cell = TrackGridCell::new(shell, &HOME_TRACK_GRID_FIELDS, play, field, None);
            let drag_title = track.title.clone();
            let drag_artwork = cell.drag_paintable_source();
            cell.bind(0, track);
            let widget = cell.widget();
            let drag_shell = Rc::downgrade(shell);
            install_compact_playlist_drag_source(&widget, &drag_artwork, move || {
                let shell = drag_shell.upgrade()?;
                let source = PlaylistTrackSource::current_track(&shell, play_key)?;
                Some((source, drag_title.clone()))
            });
            widget
        }
        HomeItem::Album(item) => {
            let mut album = item.album;
            album.title = item.title;
            if item.artwork_binding.is_some() {
                album.artwork_binding = item.artwork_binding;
            }
            let album_key = album.album_key;
            let cell = AlbumGridCell::new(shell, &HOME_ALBUM_GRID_FIELDS, None);
            let drag_title = album.title.clone();
            let drag_artwork = cell.drag_paintable_source();
            cell.bind(0, album);
            let widget = cell.widget();
            let drag_shell = Rc::downgrade(shell);
            install_compact_playlist_drag_source(&widget, &drag_artwork, move || {
                let shell = drag_shell.upgrade()?;
                let source =
                    PlaylistTrackSource::current_target(&shell, PlaybackTarget::Album(album_key))?;
                Some((source, drag_title.clone()))
            });
            widget
        }
    }
}

fn activate_home_item(shell: &Rc<Shell>, item: HomeItem) {
    match item {
        HomeItem::Track(item) => super::collections::PlaybackTarget::Track(item.track.track_key)
            .play(shell, playback::QueuePlacement::Now, false),
        HomeItem::Album(item) => shell.navigate(Route::AlbumDetail(item.album.album_key)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires a GTK display"]
    fn home_section_header_template_builds() {
        gtk::init().expect("GTK display");
        crate::application::verify_interface_resources().expect("compiled interface resources");
        let header = home_section_header("Section");
        assert!(header.first_child().is_some());
        assert!(header.previous().parent().is_some());
        assert!(header.next().parent().is_some());
    }

    #[test]
    fn home_page_start_stays_on_a_complete_available_page() {
        assert_eq!(clamped_page_start(12, 6, 14), 12);
        assert_eq!(clamped_page_start(18, 6, 14), 12);
        assert_eq!(clamped_page_start(10, 6, 0), 0);
    }

    #[test]
    fn newly_added_refreshes_authoritative_library_facts() {
        assert_eq!(
            home_refresh_action(HomeSectionKind::NewlyAdded),
            HomeRefreshAction::Library
        );
        assert_eq!(
            home_refresh_action(HomeSectionKind::MostPlayed),
            HomeRefreshAction::Section(HomeSectionKind::MostPlayed)
        );
    }

    #[test]
    fn explore_refresh_advances_variation_before_requesting_facts() {
        assert_eq!(next_home_variation(41), 42);
        assert_eq!(next_home_variation(i64::MAX), i64::MIN);
    }

    #[test]
    fn emptied_provider_section_no_longer_suppresses_the_home_empty_state() {
        assert!(visible_section_exists([true]));
        assert!(!visible_section_exists([false]));
        assert!(!visible_section_exists([]));
    }
}
