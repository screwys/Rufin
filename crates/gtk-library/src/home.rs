use gtk_widgets::media_drag::{MediaDragSource, install_compact_media_drag_source};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use library::{
    HomeAlbumRow, HomeProviderSection, HomeSectionRows, HomeTrackRow, RadioSeed, TagRow,
};
use localization::{album_count_text, msgid, tr, track_count_text};
use rufin_core::playback::RadioPlayRequest;

use crate::CatalogUi;
use gtk_widgets::artwork::presentation::stable_seed;
use gtk_widgets::controls::{ActionButtonVariant, configure_action_button};
use gtk_widgets::favorites::{
    favorite_button_is_active, favorite_icon_button, set_favorite_button_active,
};
use gtk_widgets::layout::{configure_fill_width_clip, width_allocation_owner};
use gtk_widgets::localization::{bind_label_text_with, localized_label};
use gtk_widgets::mounted_route::MountedRoute;
use rufin_core::settings::presentation::format_duration_units;
use rufin_core::settings::{HomeBlockKind, HomeSectionKind};
use rufin_core::view::HomeSection;

use super::collections::library_route_inset;
use super::detail_showcase::{
    DetailShowcaseView, MediaShowcase, detail_playback_controls, detail_radio_button,
    home_album_cover_projection, media_showcase,
};
use super::grid_cells::{
    AlbumGridCell, FixedPageCollectionRow, ReusableCollectionGridCell, TrackGridCell,
    collection_grid_column_count, fixed_page_collection_row,
};
use crate::home_layout::home_section_header;
use gtk_media_menus::media_menus::present_album_context_menu;
use gtk_widgets::selection::track_play_request;

use crate::route_layout::{ROUTE_TOP_MARGIN, route_scroller_widget};
use gtk_widgets::detail_links::{DetailLinkBinding, album_row_artist_links};
use gtk_widgets::library_fields::{COLLECTION_GRID_CARD_MARGIN, TrackPresentation};
use rufin_core::route::Route;

const HOME_SHOWCASE_ACTION_MIN_COVER_SIZE: i32 = 200;

#[derive(Clone, PartialEq)]
#[expect(
    clippy::large_enum_variant,
    reason = "Home sections are hard-bounded and keep final rows inline"
)]
enum HomeItem {
    Track(HomeTrackRow),
    Album(HomeAlbumRow),
}

type HomeItemBinding = Rc<dyn Fn(HomeItem)>;

#[derive(Clone)]
struct MountedHomeSection {
    data: Rc<RefCell<Vec<HomeItem>>>,
    model: gio::ListStore,
    row: FixedPageCollectionRow,
    previous: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    page_start: Rc<Cell<usize>>,
    page_size: Rc<Cell<usize>>,
    cells: Rc<RefCell<Vec<(u32, HomeItemBinding)>>>,
}

impl MountedHomeSection {
    fn replace(&self, items: Vec<HomeItem>) {
        let same_order = {
            let previous = self.data.borrow();
            previous.len() == items.len()
                && previous.iter().zip(&items).all(|(a, b)| match (a, b) {
                    (HomeItem::Track(a), HomeItem::Track(b)) => {
                        a.track.media_uri == b.track.media_uri
                    }
                    (HomeItem::Album(a), HomeItem::Album(b)) => {
                        a.album.media_uri == b.album.media_uri
                    }
                    _ => false,
                })
        };
        if same_order {
            let mut incoming = items.into_iter();
            self.update(|item| {
                let next = incoming.next().expect("matching Home section lengths");
                if *item == next {
                    return false;
                }
                *item = next;
                true
            });
            return;
        }
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
        self.cells.borrow_mut().clear();
        self.model.splice(0, self.model.n_items(), &additions);
        self.page_start.set(page_start);
        if let Some(previous) = self.previous.upgrade() {
            previous.set_sensitive(page_start > 0);
        }
        if let Some(next) = self.next.upgrade() {
            next.set_sensitive(page_end < data.len());
        }
    }

    fn update(&self, mut update: impl FnMut(&mut HomeItem) -> bool) {
        let changed = {
            let mut data = self.data.borrow_mut();
            data.iter_mut()
                .enumerate()
                .filter_map(|(position, item)| update(item).then(|| (position, item.clone())))
                .collect::<Vec<_>>()
        };
        let cells = self.cells.borrow().clone();
        for (position, item) in changed {
            if let Some((_, bind)) = cells
                .iter()
                .find(|(cell, _)| self.page_start.get().saturating_add(*cell as usize) == position)
            {
                bind(item);
            }
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
    header: crate::home_layout::HomeSectionHeaderView,
    mounted: MountedHomeSection,
}

/// Which section a shown Home section is, so a new read updates it in place.
#[derive(Clone, PartialEq)]
enum HomeSectionId {
    Showcase,
    Genres,
    Rows(HomeBlockKind),
    Provider(String),
}

fn home_section_id(section: &HomeSection) -> HomeSectionId {
    match section {
        HomeSection::Showcase(_) => HomeSectionId::Showcase,
        HomeSection::Genres(_) => HomeSectionId::Genres,
        HomeSection::Rows { block, .. } => HomeSectionId::Rows(*block),
        HomeSection::Provider(provider) => HomeSectionId::Provider(provider.section_id.clone()),
    }
}

/// A shown Home section: a row of items, or a widget built from the section it shows.
#[derive(Clone)]
enum ShownHomeSection {
    Rows(HomeSectionView),
    Built(gtk::Widget, HomeSection),
}

impl ShownHomeSection {
    fn root(&self) -> &gtk::Widget {
        match self {
            Self::Rows(view) => &view.root,
            Self::Built(widget, _) => widget,
        }
    }
}

#[derive(Clone)]
struct HomeRouteProjection {
    shell: std::rc::Weak<CatalogUi>,
    /// The showcase album, for its menu and download counts.
    showcase: Rc<RefCell<Option<HomeAlbumRow>>>,
    shown: Rc<RefCell<Vec<(HomeSectionId, ShownHomeSection)>>>,
    content: gtk::Box,
    empty: gtk::Widget,
}

impl HomeRouteProjection {
    /// Shows Home's sections in the order core composed them.
    fn apply(&self, sections: Vec<HomeSection>) {
        let Some(shell) = self.shell.upgrade() else {
            return;
        };
        let mut previous = self.shown.take();
        let mut shown = Vec::with_capacity(sections.len());
        let mut after = None::<gtk::Widget>;
        self.showcase.replace(None);
        for section in sections {
            let id = home_section_id(&section);
            let kept = previous
                .iter()
                .position(|(shown, _)| *shown == id)
                .map(|index| previous.remove(index).1);
            if let HomeSection::Showcase(row) = &section {
                self.showcase.replace(Some(HomeAlbumRow::clone(row)));
            }
            let next = match (kept, section) {
                (Some(ShownHomeSection::Rows(view)), HomeSection::Rows { rows, .. }) => {
                    view.mounted.replace(home_items(&rows));
                    ShownHomeSection::Rows(view)
                }
                (Some(ShownHomeSection::Rows(view)), HomeSection::Provider(provider)) => {
                    view.header.set_title(&home_provider_title(&provider));
                    view.mounted.replace(home_items(&provider.rows));
                    ShownHomeSection::Rows(view)
                }
                (Some(ShownHomeSection::Built(widget, built)), section) if built == section => {
                    ShownHomeSection::Built(widget, built)
                }
                (kept, section) => {
                    if let Some(kept) = kept {
                        self.content.remove(kept.root());
                    }
                    let next = self.build(&shell, section);
                    self.content.append(next.root());
                    next
                }
            };
            self.content
                .reorder_child_after(next.root(), after.as_ref());
            after = Some(next.root().clone());
            shown.push((id, next));
        }
        for (_, stale) in previous {
            self.content.remove(stale.root());
        }
        self.empty.set_visible(shown.is_empty());
        self.content
            .reorder_child_after(&self.empty, after.as_ref());
        self.shown.replace(shown);
    }

    fn build(&self, shell: &Rc<CatalogUi>, section: HomeSection) -> ShownHomeSection {
        match section {
            HomeSection::Rows { block, rows } => ShownHomeSection::Rows(shell.home_section_view(
                &tr(rufin_core::settings::presentation::home_block_title(block)),
                home_items(&rows),
                block.section_kind(),
            )),
            HomeSection::Provider(provider) => ShownHomeSection::Rows(shell.home_section_view(
                &home_provider_title(&provider),
                home_items(&provider.rows),
                None,
            )),
            HomeSection::Showcase(row) => ShownHomeSection::Built(
                shell.home_showcase(&row, Rc::clone(&self.showcase)),
                HomeSection::Showcase(row),
            ),
            HomeSection::Genres(genres) => {
                ShownHomeSection::Built(shell.home_genres(&genres), HomeSection::Genres(genres))
            }
        }
    }

    fn sections(&self) -> Vec<HomeSectionView> {
        self.shown
            .borrow()
            .iter()
            .filter_map(|(_, shown)| match shown {
                ShownHomeSection::Rows(view) => Some(view.clone()),
                ShownHomeSection::Built(..) => None,
            })
            .collect()
    }
}

fn home_provider_title(provider: &HomeProviderSection) -> String {
    rufin_core::settings::presentation::home_provider_title(
        &provider.section_id,
        provider.title.as_deref(),
    )
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

impl CatalogUi {
    pub fn home_route(self: &Rc<Self>, sections: Vec<HomeSection>) -> MountedRoute {
        let scroller = gtk::ScrolledWindow::new();
        configure_fill_width_clip(&scroller, gtk::PolicyType::Automatic);
        scroller.set_vexpand(true);

        let content = gtk::Box::new(gtk::Orientation::Vertical, 18);
        content.add_css_class("route-content");
        content.set_hexpand(true);
        content.set_halign(gtk::Align::Fill);
        content.set_width_request(1);
        content.set_margin_top(ROUTE_TOP_MARGIN);
        let empty = crate::route_layout::route_empty_view(msgid("Nothing here yet"));
        content.append(&empty);
        let inset_content = library_route_inset(content.clone().upcast());
        scroller.set_child(Some(&inset_content));

        let projection = HomeRouteProjection {
            shell: Rc::downgrade(self),
            showcase: Rc::new(RefCell::new(None)),
            shown: Rc::new(RefCell::new(Vec::new())),
            content,
            empty,
        };
        projection.apply(sections);
        let widget = route_scroller_widget(scroller);
        let apply = projection.clone();
        let download = projection.clone();

        let download_counts = projection.clone();

        let counts = crate::collection_download_change(move |target, downloaded| {
            let rufin_core::collection_context::CollectionTarget::Album(uri) = target else {
                return;
            };
            if let Some(showcase) = &mut *download_counts.showcase.borrow_mut()
                && showcase.album.media_uri == *uri
            {
                showcase.album.summary.downloaded_count = if downloaded {
                    showcase.album.summary.track_count
                } else {
                    0
                };
            }
            for view in download_counts.sections() {
                view.mounted.update(|item| {
                    if let HomeItem::Album(item) = item
                        && item.album.media_uri == *uri
                        && (item.album.summary.downloaded_count == item.album.summary.track_count)
                            != downloaded
                    {
                        item.album.summary.downloaded_count = if downloaded {
                            item.album.summary.track_count
                        } else {
                            0
                        };
                        true
                    } else {
                        false
                    }
                });
            }
        });
        MountedRoute::new(widget, Rc::new(|| {}))
            .with_home_page_apply(Rc::new(move |sections| apply.apply(sections)))
            .with_download_change(Rc::new(move |event| {
                let downloads::DownloadEvent::Changed {
                    media_uri: uri,
                    downloaded,
                } = event
                else {
                    return;
                };
                let (uri, downloaded) = (uri.as_str(), *downloaded);
                for view in download.sections() {
                    view.mounted.update(|item| {
                        if let HomeItem::Track(item) = item
                            && item.track.media_uri == uri
                            && item.track.is_downloaded != downloaded
                        {
                            item.track.is_downloaded = downloaded;
                            true
                        } else {
                            false
                        }
                    });
                }
            }))
            .with_download_change(counts)
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

        let content_width = crate::route_layout::home_album_content_width_for((self.route_width)());
        let page_size = collection_grid_column_count(content_width);
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let widget_shell = Rc::clone(self);
        let activate_shell = Rc::clone(self);
        let cells = Rc::new(RefCell::new(Vec::new()));
        let bind_cells = Rc::clone(&cells);
        let row = fixed_page_collection_row(
            model.clone(),
            page_size,
            move |position, item: HomeItem| {
                let (widget, bind) = home_item_widget(&widget_shell, item);
                bind_cells.borrow_mut().push((position, bind));
                widget
            },
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
            cells,
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
            let refresh_section = mounted.clone();
            header.refresh().connect_clicked(move |_| {
                refresh_section.page_start.set(0);
                (shell.refresh_home_section)(kind);
            });
        } else {
            header.refresh().set_visible(false);
        }
        HomeSectionView {
            root: section.upcast(),
            header,
            mounted,
        }
    }

    fn home_showcase(
        self: &Rc<Self>,
        item: &HomeAlbumRow,
        showcase: Rc<RefCell<Option<HomeAlbumRow>>>,
    ) -> gtk::Widget {
        let width = crate::route_layout::home_album_content_width_for((self.route_width)());
        let mut album = item.album.clone();
        album.title.clone_from(&item.title);
        let seed = stable_seed(&album.object_id);
        let cover = home_album_cover_projection(
            self,
            &album,
            crate::route_layout::detail_showcase_cover_size(width),
        );
        let title_text = album.title.clone();
        let showcase_view =
            DetailShowcaseView::new("home-showcase", seed, "Showcase", true, &title_text);
        let radio = detail_radio_button();
        let radio_controller = self.playback.clone();
        let radio_seed = RadioSeed::Album(album.album_key);
        radio.connect_clicked(move |_| {
            radio_controller.play_radio(RadioPlayRequest::now(radio_seed.clone()));
        });
        showcase_view.append_kind_control(&radio);
        let drag_album = album.clone();
        let drag_title = title_text.clone();
        let drag_target: gtk::Widget = cover.button().upcast();
        let drag_artwork = cover.drag_paintable_source();
        let drag_shell = Rc::downgrade(self);
        install_compact_media_drag_source(&drag_target, &drag_artwork, move || {
            let _shell = drag_shell.upgrade()?;
            let source = MediaDragSource::capture_target(
                gtk_widgets::selection::album_play_request(&drag_album),
            );
            Some((source, drag_title.clone()))
        });

        let actions = showcase_view.actions();
        actions.set_halign(gtk::Align::Start);
        let play_shell = Rc::clone(self);
        let play_target = gtk_widgets::selection::album_play_request(&album);
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            (play_shell.media_menus.play_target)(&play_target, placement, shuffled);
        });
        let controls = detail_playback_controls(
            &actions,
            msgid("Play album"),
            Some(album.state.favorite),
            play,
        );
        let favorite = favorite_icon_button("Favorite");
        configure_action_button(&favorite, ActionButtonVariant::DetailFavorite);
        set_favorite_button_active(&favorite, album.state.favorite);
        actions.append(&favorite);
        let hover_favorite = controls
            .favorite
            .as_ref()
            .expect("Home album has a Favorite cover control")
            .clone();
        for button in [favorite, hover_favorite] {
            self.register_favorite_button(
                gtk_widgets::favorites::album_favorite_key(&album.media_uri),
                &button,
            );
            let favorite_shell = Rc::clone(self);
            let favorite_media_uri = album.media_uri.clone();
            button.connect_clicked(move |button| {
                favorite_shell.set_favorite(
                    library::FavoriteTarget::Album(favorite_media_uri.clone()),
                    !favorite_button_is_active(button),
                );
            });
        }

        let menu_shell = Rc::clone(self);
        let context_menu = Rc::new(move |target: &gtk::Widget, position| {
            let album = showcase.borrow().as_ref().map(|item| item.album.clone());
            let Some(album) = album else {
                return;
            };
            present_album_context_menu(
                target,
                &menu_shell.media_menus,
                album,
                None,
                None,
                position,
            );
        });

        let (artist_scroll, artist) = gtk_widgets::text_slide::new_label();
        artist.set_text(&album.artist);
        artist.add_css_class("detail-artist");
        artist.set_xalign(0.0);
        artist.set_halign(gtk::Align::Start);
        artist.set_width_request(1);
        artist.set_width_chars(1);
        DetailLinkBinding::new(&artist, self.route_navigation())
            .bind(album_row_artist_links(&album));
        showcase_view.append_detail(&artist_scroll);
        showcase_view.replace_summary(&[
            (
                "rufin-x-office-calendar-symbolic",
                album.year.map(|year| year.to_string()).unwrap_or_default(),
            ),
            (
                "rufin-tracks-symbolic",
                track_count_text(album.summary.track_count.max(0) as u64),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units((album.summary.duration_millis.max(0) / 1_000) as u32),
            ),
        ]);
        let track_count = album.summary.track_count.max(0) as u64;
        showcase_view.bind_summary_text_with(1, move || track_count_text(track_count));

        let showcase = media_showcase(MediaShowcase {
            view: showcase_view,
            initial_width: width,
            cover,
            cover_controls: controls,
            context_menu: Some(context_menu),
            actions_min_cover_size: Some(HOME_SHOWCASE_ACTION_MIN_COVER_SIZE),
        });
        showcase.upcast()
    }

    fn home_genres(self: &Rc<Self>, genres: &[TagRow]) -> gtk::Widget {
        let section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        section.set_hexpand(true);
        let heading = localized_label(rufin_core::settings::presentation::home_block_title(
            HomeBlockKind::Genres,
        ));
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
        section.upcast()
    }

    fn home_genre_chip(self: &Rc<Self>, genre: &TagRow) -> gtk::Widget {
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
        let album_count = genre.summary.album_count.max(0) as u64;
        let track_count = genre.summary.track_count.max(0) as u64;
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
        let key = library::GenreKey::from_raw(genre.key);
        button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(key)));
        button.upcast()
    }
}

fn home_item_widget(shell: &Rc<CatalogUi>, item: HomeItem) -> (gtk::Widget, HomeItemBinding) {
    match item {
        HomeItem::Track(item) => {
            let mut track = item.track;
            track.title = item.title;
            let play_shell = Rc::clone(shell);
            let play_key = track.track_key;
            let play = Rc::new(move |_| {
                (play_shell.media_menus.play_target)(
                    &track_play_request(play_key),
                    rufin_core::playback::QueuePlacement::Now,
                    false,
                );
            });
            let field = Rc::new(move |_, track: &library::TrackRow, field| {
                TrackPresentation::links(track, field)
            });
            let cell = TrackGridCell::new(
                shell,
                &rufin_core::settings::layout::HOME_TRACK_FIELDS,
                play,
                field,
                None,
            );
            cell.follow_favorites();
            cell.bind(0, track);
            (
                cell.widget(),
                Rc::new(move |item| {
                    if let HomeItem::Track(item) = item {
                        let mut track = item.track;
                        track.title = item.title;
                        cell.bind(0, track);
                    }
                }),
            )
        }
        HomeItem::Album(item) => {
            let mut album = item.album;
            album.title = item.title;
            let cell = AlbumGridCell::new(
                shell,
                &rufin_core::settings::layout::HOME_ALBUM_FIELDS,
                None,
            );
            cell.follow_favorites();
            cell.bind(0, album);
            (
                cell.widget(),
                Rc::new(move |item| {
                    if let HomeItem::Album(item) = item {
                        let mut album = item.album;
                        album.title = item.title;
                        cell.bind(0, album);
                    }
                }),
            )
        }
    }
}

fn activate_home_item(shell: &Rc<CatalogUi>, item: HomeItem) {
    match item {
        HomeItem::Track(item) => (shell.media_menus.play_target)(
            &track_play_request(item.track.track_key),
            rufin_core::playback::QueuePlacement::Now,
            false,
        ),
        HomeItem::Album(item) => shell.navigate(Route::AlbumDetail(item.album.media_uri.clone())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_page_start_stays_on_a_complete_available_page() {
        assert_eq!(clamped_page_start(12, 6, 14), 12);
        assert_eq!(clamped_page_start(18, 6, 14), 12);
        assert_eq!(clamped_page_start(10, 6, 0), 0);
    }
}

use gtk_media_menus::media_menus::CollectionPlay;
