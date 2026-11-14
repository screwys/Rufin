use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use ::library::AlbumView;
use adw::prelude::*;

use crate::library::CatalogUi;
use crate::library::LibraryListKey;
use crate::widgets::mounted_route::MountedRoute;
use crate::widgets::view_model::Rows;
use localization::{msgid, track_count_text};
use rufin_core::settings::presentation::format_duration_units;
use rufin_core::view::Opened;

use super::collections::library_route_inset;
use super::detail_showcase::{
    DetailShowcaseView, ShowcaseCover, ShowcaseMount, ShowcaseShape, album_external_links,
    media_cover_projection,
};
use crate::library::route_layout::{
    PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, detail_route_wrapper,
};
use crate::menus::media_menus::present_row_menu;
use crate::widgets::detail_links::DetailLinkBinding;
use rufin_core::route::detail_links::album_artist_links;

impl CatalogUi {
    pub fn album_detail_view(self: &Rc<Self>, mut read: Opened) -> MountedRoute {
        let (false, Some(::library::Header::Album(album))) =
            (read.view.is_missing(), read.header.take())
        else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                "Album",
                msgid("This isn't available"),
            ));
        };
        let album = *album;
        let current_album = Rc::new(RefCell::new(album.clone()));

        let wrapper = detail_route_wrapper(22);
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);

        let inner_content_width =
            crate::library::route_layout::detail_route_inner_width_for_viewport(
                self.route_width(),
                PRIMARY_ROUTE_MARGIN_START,
            );
        let rows = Rows::over_view(self.products.runtime.clone(), &mut read);
        rows.list_model()
            .set_disc_titles(album.metadata.disc_titles.clone());
        let (tracks_widget, track_projection, track_toolbar) = self.scrolling_track_projection(
            rows.clone(),
            LibraryListKey::AlbumDetailTracks,
            "album-detail",
        );
        track_toolbar.set_layout_control_visible(false);
        let cover = media_cover_projection(
            self,
            crate::widgets::library_fields::opaque_artwork(album.row.artwork_binding.as_deref()),
            ShowcaseShape::Detail.cover_size(inner_content_width),
            "album-detail-cover",
        );
        let showcase_view = DetailShowcaseView::new(&album.metadata.title);
        showcase_view.replace_summary(&album_summary_items(&album));
        let track_count = Rc::new(Cell::new(album.row.summary.track_count.max(0) as u32));
        let localized_track_count = Rc::clone(&track_count);
        showcase_view.bind_summary_text_with(1, move || {
            track_count_text(u64::from(localized_track_count.get()))
        });
        showcase_view.set_genres(
            self,
            album
                .row
                .links
                .genres
                .iter()
                .map(|genre| (genre.name.as_str(), genre.genre_key)),
        );

        let artist = showcase_view.set_detail(&album.metadata.artist);
        let artist_links = DetailLinkBinding::new(&artist, self.route_navigation());
        artist_links.bind(album_artist_links(&album));

        let play_controller = self.products.playback.clone();
        let play_rows = rows.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            play_rows.play_all(&play_controller, placement, shuffled);
        });
        let menu_shell = Rc::clone(self);
        let menu_album = Rc::clone(&current_album);
        let menu_play = Rc::clone(&play);
        let menu: crate::widgets::interactions::ContextMenuOpen =
            Rc::new(move |target, position| {
                present_row_menu(
                    target,
                    menu_shell.shell(),
                    library::Row::Album(menu_album.borrow().row.clone()),
                    Some(Rc::clone(&menu_play)),
                    position,
                );
            });

        showcase_view.replace_external_links(album_external_links(self, &album));
        let showcase = showcase_view.mount(
            self,
            library::Row::Album(album.row.clone()),
            ShowcaseMount {
                shape: ShowcaseShape::Detail,
                initial_width: inner_content_width,
                cover: ShowcaseCover::Single(cover.clone()),
                play_label: msgid("Play album"),
                play,
                menu,
                download: true,
            },
        );
        wrapper.append(&library_route_inset(showcase));
        wrapper.append(&tracks_widget);
        let item_navigation = track_projection.item_navigation();
        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("content"));
        route_stack.add_named(
            &crate::library::route_layout::placeholder_view("Album", msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("content");

        // A reread, or a state change, shows the header it read; an album that is gone reads
        // as missing.
        let show: crate::widgets::mounted_route::MountedHeader = {
            let shell = Rc::downgrade(self);
            let album = Rc::clone(&current_album);
            let showcase = showcase_view.clone();
            let route_stack = route_stack.clone();
            let list = rows.list_model().downgrade();
            Rc::new(move |header| {
                let (Some(shell), Some(::library::Header::Album(next))) = (shell.upgrade(), header)
                else {
                    route_stack.set_visible_child_name("missing");
                    return;
                };
                route_stack.set_visible_child_name("content");
                if let Some(list) = list.upgrade() {
                    list.set_disc_titles(next.metadata.disc_titles.clone());
                }
                showcase.show_row(library::Row::Album(next.row.clone()));
                showcase.set_title(&next.metadata.title);
                showcase.replace_summary(&album_summary_items(&next));
                track_count.set(next.row.summary.track_count.max(0) as u32);
                showcase.replace_external_links(album_external_links(&shell, &next));
                showcase.set_genres(
                    &shell,
                    next.row
                        .links
                        .genres
                        .iter()
                        .map(|genre| (genre.name.as_str(), genre.genre_key)),
                );
                cover.replace(
                    &shell,
                    crate::widgets::library_fields::opaque_artwork(
                        next.row.artwork_binding.as_deref(),
                    ),
                );
                artist.set_text(&next.metadata.artist);
                artist_links.bind(album_artist_links(&next));
                album.replace(*next);
            })
        };
        let reads = track_projection.page_reads(|_| {}, {
            let show = Rc::clone(&show);
            move |header| show(header)
        });
        let layout_cycle = track_toolbar.layout_cycle();
        let resume = {
            let shell = Rc::downgrade(self);
            let track_projection = track_projection.clone();
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::AlbumDetailTracks);
                track_projection
                    .apply_library_list_settings(LibraryListKey::AlbumDetailTracks, &settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        MountedRoute::new(route_stack.upcast(), resume)
            .with_rows(&rows)
            .with_header(
                move || {
                    Some(::library::Header::Album(Box::new(
                        current_album.borrow().clone(),
                    )))
                },
                show,
            )
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
            .with_search(track_projection.search())
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(item_navigation)
            .with_initial_demand(Rc::new(move || rows.resume_initial_demand()))
    }
}

fn album_summary_items(summary: &AlbumView) -> Vec<(&'static str, String)> {
    vec![
        (
            "rufin-x-office-calendar-symbolic",
            rufin_core::settings::presentation::album_year(&summary.row),
        ),
        (
            "rufin-tracks-symbolic",
            track_count_text(summary.row.summary.track_count.max(0) as u64),
        ),
        (
            "rufin-preferences-system-time-symbolic",
            format_duration_units((summary.row.summary.duration_millis.max(0) / 1_000) as u32),
        ),
    ]
}

use crate::menus::media_menus::CollectionPlay;
