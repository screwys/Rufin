use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use ::library::{AlbumDetail, AlbumRow, FavoriteTarget};
use adw::prelude::*;

use crate::LibraryListKey;
use crate::favorites::{
    album_favorite_key, favorite_button_is_active, favorite_icon_button, set_favorite_button_active,
};
use crate::format_duration_units;
use crate::shell::Shell;
use crate::shell::actions::{ActionButtonVariant, configure_action_button};
use crate::shell::route::{LatestMountedRouteRead, MountedRoute};
use ::library::RadioSeed;
use localization::{msgid, track_count_text};
use playback::RadioPlayRequest;

use super::collection_context::present_album_context_menu;
use super::collections::CollectionPlay;
use super::collections::{library_route_inset, set_library_table_content_height};
use super::detail_links::{DetailLinkBinding, album_artist_links};
use super::detail_showcase::{
    DetailShowcaseView, MediaShowcase, album_external_links, detail_genre_pill_button,
    detail_playback_controls, detail_radio_button, detail_showcase_frame_with_back,
    fit_detail_text, media_cover_projection, media_showcase,
};
use super::release_kind::album_release_kind_label;
use super::route::Route;
use super::route_layout::{
    PRIMARY_ROUTE_MARGIN_END, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
    detail_route_inner_width, detail_route_scroller, detail_route_wrapper,
    detail_showcase_cover_size,
};
use super::routes::SearchableTrackOptions;
use super::track_model::{PreparedTrackProjection, TrackProjectionRequest};

const ALBUM_DETAIL_ROUTE_INSET: i32 = PRIMARY_ROUTE_MARGIN_START + PRIMARY_ROUTE_MARGIN_END;

#[derive(Clone)]
struct AlbumDetailReadRequest {
    tracks: TrackProjectionRequest,
}

impl Shell {
    pub(crate) fn album_detail_view(
        self: &Rc<Self>,
        detail: Option<AlbumDetail>,
        first_row_position: usize,
        first_rows: Vec<library::TrackRow>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view("Album", msgid("This isn't available")),
            );
        };
        let album = detail.album.clone();
        let album_id = album.album_key;
        let album_uri = album.media_uri.clone();
        let tracks = detail.track_order.clone();
        let current_album = Rc::new(RefCell::new(detail.album.clone()));
        let context_id = format!("album:{album_id}");
        let applied_external_link_settings = Rc::new(RefCell::new(
            self.settings.current.borrow().external_site_links.clone(),
        ));

        let wrapper = detail_route_wrapper(0);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 22);
        content.set_margin_top(ROUTE_TOP_MARGIN);
        content.set_hexpand(true);
        content.set_halign(gtk::Align::Fill);
        content.set_width_request(1);

        let inner_content_width = detail_route_inner_width(self, PRIMARY_ROUTE_MARGIN_START);
        let table_scroller = gtk::ScrolledWindow::new();
        table_scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
        table_scroller.set_width_request(1);
        table_scroller.set_min_content_width(0);
        table_scroller.set_max_content_width(1);
        table_scroller.set_propagate_natural_width(false);
        table_scroller.set_propagate_natural_height(false);
        table_scroller.set_hexpand(true);
        table_scroller.set_halign(gtk::Align::Fill);
        let resize_scroller = table_scroller.clone();
        let resize_tracks: Rc<dyn Fn(usize)> = Rc::new(move |row_count| {
            set_library_table_content_height(&resize_scroller, row_count, None);
        });
        let track_projection = self.searchable_track_collection(
            tracks,
            first_row_position,
            first_rows,
            LibraryListKey::AlbumDetailTracks,
            SearchableTrackOptions {
                on_visible_count_changed: Some(resize_tracks),
                context_id: context_id.clone(),
                content_inset: ALBUM_DETAIL_ROUTE_INSET,
                fixed_layout: None,
                search: None,
            },
        );
        let cover_size = detail_showcase_cover_size(inner_content_width);
        let cover = media_cover_projection(
            self,
            super::library_fields::opaque_artwork(detail.album.artwork_binding.as_deref()),
            cover_size,
            "album-detail-cover",
        );
        let showcase_view = DetailShowcaseView::new(
            "album-detail-showcase",
            album
                .object_id
                .bytes()
                .fold(2_166_136_261_u32, |hash, byte| {
                    hash.wrapping_mul(16_777_619) ^ u32::from(byte)
                }),
            album_release_kind_label(&album),
            true,
            &album.title,
        );
        showcase_view.add_external_links_class("album-detail-link-stack");
        showcase_view.replace_summary(&album_summary_items(&detail.album));
        let track_count = Rc::new(Cell::new(detail.album.track_count.max(0) as u32));
        let localized_track_count = Rc::clone(&track_count);
        showcase_view.bind_summary_text_with(1, move || {
            track_count_text(u64::from(localized_track_count.get()))
        });

        let radio = detail_radio_button();
        let radio_controller = self.products.playback.radio.clone();
        let radio_album = Rc::clone(&current_album);
        radio.connect_clicked(move |_| {
            radio_controller.play_radio(RadioPlayRequest::now(RadioSeed::Album(
                radio_album.borrow().album_key,
            )));
        });
        showcase_view.append_kind_control(&radio);
        self.append_album_genre_buttons(&showcase_view, &album.genres);

        let artist = gtk::Label::new(Some(&album.display_artist));
        artist.add_css_class("detail-artist");
        artist.set_xalign(0.0);
        artist.set_halign(gtk::Align::Start);
        artist.set_wrap(true);
        artist.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        artist.set_width_request(1);
        artist.set_width_chars(1);
        artist.set_max_width_chars(32);
        fit_detail_text(&artist, &album.display_artist);
        let artist_links = DetailLinkBinding::new(&artist, self);
        artist_links.bind(album_artist_links(&album));
        showcase_view.append_detail(&artist);

        let actions = showcase_view.actions();
        actions.add_css_class("album-detail-actions");
        actions.set_halign(gtk::Align::Start);
        let play_controller = self.products.playback.queue.clone();
        let play_tracks = track_projection.clone();
        let play_context_id = context_id.clone();
        let play: CollectionPlay = Rc::new(move |placement| {
            play_tracks.play_source(play_controller.clone(), placement, play_context_id.clone());
        });
        let cover_controls = detail_playback_controls(
            &actions,
            msgid("Play album"),
            Some(album.favorite),
            true,
            Rc::clone(&play),
        );

        let favorite = favorite_icon_button("Favorite");
        configure_action_button(&favorite, ActionButtonVariant::DetailFavorite);
        set_favorite_button_active(&favorite, album.favorite);
        actions.append(&favorite);
        let hover_favorite = cover_controls
            .favorite
            .as_ref()
            .expect("album detail has a Favorite cover control")
            .clone();
        let favorite_media_uri = album.media_uri.clone();
        for button in [favorite, hover_favorite] {
            self.register_favorite_button(album_favorite_key(&favorite_media_uri), &button);
            let shell = Rc::clone(self);
            let favorite_media_uri = favorite_media_uri.clone();
            button.connect_clicked(move |button| {
                shell.set_favorite_with_feedback(
                    FavoriteTarget::Album(favorite_media_uri.clone()),
                    !favorite_button_is_active(button),
                    Some(button),
                );
            });
        }

        let menu_shell = Rc::clone(self);
        let menu_album = Rc::clone(&current_album);
        let menu_play = Rc::clone(&play);
        let context_menu: crate::interactions::ContextMenuOpen =
            Rc::new(move |target, position| {
                let album = menu_album.borrow().clone();
                present_album_context_menu(
                    target,
                    &menu_shell,
                    album,
                    None,
                    Some(Rc::clone(&menu_play)),
                    position,
                );
            });

        showcase_view.replace_external_links(album_external_links(self, &album));
        let showcase = detail_showcase_frame_with_back(
            self,
            media_showcase(MediaShowcase {
                view: showcase_view.clone(),
                initial_width: inner_content_width,
                cover: cover.clone(),
                cover_controls,
                context_menu: Some(context_menu),
                actions_min_cover_size: None,
            }),
        );
        content.append(&showcase);

        let table = gtk::Box::new(gtk::Orientation::Vertical, 10);
        table.set_widget_name("album-detail");
        table.set_hexpand(true);
        table.set_halign(gtk::Align::Fill);
        table.set_width_request(1);
        let track_toolbar = self.library_toolbar_projection(
            LibraryListKey::AlbumDetailTracks,
            track_projection.search(),
        );
        table.append(&track_toolbar.widget());
        let item_navigation = track_projection.item_navigation();
        let track_content = track_projection.mount_in_scroller(&table_scroller);
        table.append(&track_content);
        content.append(&table);
        wrapper.append(&detail_route_scroller(library_route_inset(
            content.upcast(),
        )));
        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("content"));
        route_stack.add_named(
            &self.placeholder_view("Album", msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("content");

        let apply = {
            let shell = Rc::downgrade(self);
            let track_projection = track_projection.clone();
            let route = Route::AlbumDetail(album_uri.clone());
            Rc::new(
                move |_: AlbumDetailReadRequest,
                      result: Result<PreparedTrackProjection, String>| {
                    let Some(shell) = shell.upgrade() else {
                        return;
                    };
                    if shell.navigation.routes.borrow().current() != &route {
                        return;
                    }
                    match result {
                        Ok(prepared) => {
                            track_projection.replace_prepared(prepared);
                        }
                        Err(error) => tracing::warn!(%error, "failed to read Album Track order"),
                    }
                },
            )
        };
        let database = Arc::clone(&self.products.library);
        let source = album.source_key;
        let folder = None;
        let load = Arc::new(move |request: AlbumDetailReadRequest| {
            let database = Arc::clone(&database);
            Box::pin(async move {
                let cancellation = library::ReadCancellation::new();
                let page = database
                    .album_track_route_page(
                        source,
                        album_id,
                        folder,
                        &request.tracks.query,
                        request.tracks.settings.sort_key.track_sort(),
                        request.tracks.settings.descending,
                        library::RouteSeedWindow::top(),
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(PreparedTrackProjection {
                    order: page.order,
                    first_row_position: page.first_row_position,
                    first_rows: page.first_rows,
                    request: request.tracks,
                })
            }) as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        });
        let read = LatestMountedRouteRead::new_with_request(
            self.products.runtime.clone(),
            apply,
            load,
            "mounted Album route",
        );
        {
            let read = Rc::downgrade(&read);
            track_projection.connect_search_request(move |tracks| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with(AlbumDetailReadRequest { tracks });
            });
        }
        let layout_cycle = track_toolbar.layout_cycle();
        let resume = {
            let shell = Rc::downgrade(self);
            let album = Rc::clone(&current_album);
            let showcase = showcase_view.clone();
            let applied_external_link_settings = Rc::clone(&applied_external_link_settings);
            let track_projection = track_projection.clone();
            let read = Rc::clone(&read);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let external_link_settings =
                    shell.settings.current.borrow().external_site_links.clone();
                if *applied_external_link_settings.borrow() != external_link_settings {
                    showcase.replace_external_links(album_external_links(&shell, &album.borrow()));
                    applied_external_link_settings.replace(external_link_settings);
                }
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::AlbumDetailTracks);
                track_projection
                    .apply_library_list_settings(LibraryListKey::AlbumDetailTracks, &settings);
                track_toolbar.apply(LibraryListKey::AlbumDetailTracks, &settings);
                read.request_with(AlbumDetailReadRequest {
                    tracks: track_projection.projection_request(),
                });
            })
        };
        let download_target = current_album.borrow().media_uri.clone();
        let download_album = Rc::clone(&current_album);
        let downloads = self.collection_download_change(move |identity, downloaded| {
            if identity.strip_prefix("album:") == Some(download_target.as_str()) {
                let mut row = download_album.borrow_mut();
                row.downloaded_count = if downloaded { row.track_count } else { 0 };
            }
        });
        MountedRoute::new(route_stack.upcast(), resume)
            .with_download_change(downloads)
            .with_download_change(track_projection.download_change())
            .with_search(track_projection.search())
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(item_navigation)
            .with_initial_demand({
                let track_projection = track_projection.clone();
                Rc::new(move || track_projection.resume_initial_demand())
            })
    }

    fn append_album_genre_buttons(
        self: &Rc<Self>,
        showcase: &DetailShowcaseView,
        genres: &[::library::AlbumGenreLink],
    ) {
        for genre in genres.iter().filter(|genre| !genre.name.trim().is_empty()) {
            let button = detail_genre_pill_button(genre.name.trim());
            let shell = Rc::clone(self);
            let genre_id = genre.genre_key;
            button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(genre_id)));
            showcase.append_kind_control(&button);
        }
    }
}

fn album_summary_items(summary: &AlbumRow) -> Vec<(&'static str, String)> {
    vec![
        (
            "rufin-x-office-calendar-symbolic",
            summary
                .year
                .map(|year| year.to_string())
                .unwrap_or_default(),
        ),
        (
            "rufin-tracks-symbolic",
            track_count_text(summary.track_count.max(0) as u64),
        ),
        (
            "rufin-preferences-system-time-symbolic",
            format_duration_units((summary.duration_millis.max(0) / 1_000) as u32),
        ),
    ]
}
