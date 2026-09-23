use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use ::library::{AlbumRow, FavoriteTarget};
use adw::prelude::*;

use crate::CatalogUi;
use crate::LibraryListKey;
use ::library::RadioSeed;
use gtk_widgets::controls::{ActionButtonVariant, configure_action_button};
use gtk_widgets::favorites::{
    album_favorite_key, favorite_button_is_active, favorite_icon_button, set_favorite_button_active,
};
use gtk_widgets::format_duration_units;
use gtk_widgets::mounted_route::MountedRoute;
use localization::{msgid, track_count_text};
use playback::RadioPlayRequest;

use super::collections::library_route_inset;
use super::detail_showcase::{
    DetailShowcaseView, MediaShowcase, album_external_links, detail_genre_pill_button,
    detail_playback_controls, detail_radio_button, detail_showcase_frame, fit_detail_text,
    media_cover_projection, media_showcase,
};
use crate::release_kind::album_release_kind_label;
use crate::route_layout::{
    PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, detail_route_wrapper, detail_showcase_cover_size,
};
use crate::track_model::{PreparedTrackProjection, TrackCollectionModel, TrackProjectionRequest};
use gtk_media_menus::media_menus::present_album_context_menu;
use gtk_widgets::detail_links::{DetailLinkBinding, album_artist_links};
use gtk_widgets::route::Route;

impl CatalogUi {
    pub fn album_detail_view(
        self: &Rc<Self>,
        detail: Option<(AlbumRow, library::TrackRoutePage)>,
    ) -> MountedRoute {
        let Some((album, page)) = detail else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                "Album",
                msgid("This isn't available"),
            ));
        };
        let album_id = album.album_key;
        let current_album = Rc::new(RefCell::new(album.clone()));
        let context_id = format!("album:{album_id}");
        let applied_external_link_settings = Rc::new(RefCell::new(
            self.settings.current.borrow().external_site_links.clone(),
        ));

        let wrapper = detail_route_wrapper(22);
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);

        let inner_content_width = crate::route_layout::detail_route_inner_width_for_viewport(
            (self.route_width)(),
            PRIMARY_ROUTE_MARGIN_START,
        );
        let model = TrackCollectionModel::new(
            Arc::clone(&self.library),
            self.runtime.clone(),
            page,
            self.settings
                .current
                .borrow()
                .library_list(LibraryListKey::AlbumDetailTracks),
        );
        let (tracks_widget, track_projection, track_toolbar) = self.scrolling_track_projection(
            model,
            LibraryListKey::AlbumDetailTracks,
            "album-detail",
            context_id.clone(),
        );
        track_toolbar.set_layout_control_visible(false);
        let cover_size = detail_showcase_cover_size(inner_content_width);
        let cover = media_cover_projection(
            self,
            gtk_widgets::library_fields::opaque_artwork(album.artwork_binding.as_deref()),
            cover_size,
            "album-detail-cover",
        );
        let showcase_view = DetailShowcaseView::new(
            "album-detail-showcase",
            gtk_widgets::artwork::presentation::stable_seed(&album.object_id),
            album_release_kind_label(&album),
            true,
            &album.title,
        );
        showcase_view.add_external_links_class("album-detail-link-stack");
        showcase_view.replace_summary(&album_summary_items(&album));
        let track_count = Rc::new(Cell::new(album.track_count.max(0) as u32));
        let localized_track_count = Rc::clone(&track_count);
        showcase_view.bind_summary_text_with(1, move || {
            track_count_text(u64::from(localized_track_count.get()))
        });

        let radio = detail_radio_button();
        let radio_controller = self.radio.clone();
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
        let artist_links = DetailLinkBinding::new(&artist, self.route_navigation());
        artist_links.bind(album_artist_links(&album));
        showcase_view.append_detail(&artist);

        let actions = showcase_view.actions();
        actions.add_css_class("album-detail-actions");
        actions.set_halign(gtk::Align::Start);
        let play_controller = self.queue.clone();
        let play_tracks = track_projection.clone();
        let play_context_id = context_id.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            play_tracks.play_source(
                play_controller.clone(),
                placement,
                play_context_id.clone(),
                shuffled,
            );
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
        let context_menu: gtk_widgets::interactions::ContextMenuOpen =
            Rc::new(move |target, position| {
                let album = menu_album.borrow().clone();
                present_album_context_menu(
                    target,
                    &menu_shell.media_menus,
                    album,
                    None,
                    Some(Rc::clone(&menu_play)),
                    position,
                );
            });

        showcase_view.replace_external_links(album_external_links(self, &album));
        let showcase = detail_showcase_frame(media_showcase(MediaShowcase {
            view: showcase_view.clone(),
            initial_width: inner_content_width,
            cover: cover.clone(),
            cover_controls,
            context_menu: Some(context_menu),
            actions_min_cover_size: None,
        }));
        wrapper.append(&library_route_inset(showcase));
        wrapper.append(&tracks_widget);
        let item_navigation = track_projection.item_navigation();
        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("content"));
        route_stack.add_named(
            &crate::route_layout::placeholder_view("Album", msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("content");

        let database = Arc::clone(&self.library);
        let source = album.source_key;
        let folder = None;
        let load = move |request: TrackProjectionRequest, cancellation| {
            let database = Arc::clone(&database);
            async move {
                let page = database
                    .query_track_route_page(
                        &library::TrackQuery {
                            source: source,
                            collection: Some(library::QueueCollection::AlbumKey(album_id)),
                            folder: folder,
                            favorites_only: false,
                        },
                        &request.query,
                        request.settings.sort_key.track_sort(),
                        request.settings.descending,
                        library::RouteSeedWindow::top(),
                        &cancellation,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                Ok::<_, String>(PreparedTrackProjection::from_page(page, request))
            }
        };
        let refresh =
            track_projection.connect_read(self, |request| request, load, "mounted Album route");
        let layout_cycle = track_toolbar.layout_cycle();
        let resume = {
            let shell = Rc::downgrade(self);
            let album = Rc::clone(&current_album);
            let showcase = showcase_view.clone();
            let applied_external_link_settings = Rc::clone(&applied_external_link_settings);
            let track_projection = track_projection.clone();
            let refresh = Rc::clone(&refresh);
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
                let previous = track_projection.projection_request();
                track_projection
                    .apply_library_list_settings(LibraryListKey::AlbumDetailTracks, &settings);
                track_toolbar.apply(LibraryListKey::AlbumDetailTracks, &settings);
                let tracks = track_projection.projection_request();
                if !previous.same_query(&tracks) {
                    refresh();
                }
            })
        };
        let download_target = current_album.borrow().media_uri.clone();
        let download_album = Rc::clone(&current_album);
        let downloads = crate::collection_download_change(move |identity, downloaded| {
            if identity.strip_prefix("album:") == Some(download_target.as_str()) {
                let mut row = download_album.borrow_mut();
                row.downloaded_count = if downloaded { row.track_count } else { 0 };
            }
        });
        MountedRoute::new(route_stack.upcast(), resume)
            .with_catalog_refresh(refresh)
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

use gtk_media_menus::media_menus::CollectionPlay;

pub async fn load_album_detail(
    database: &library::Database,
    album_uri: &str,
    settings: &rufin_core::settings::LibraryListSettings,
    window: library::RouteSeedWindow,
    cancellation: &library::ReadCancellation,
) -> library::LibraryResult<Option<(AlbumRow, library::TrackRoutePage)>> {
    let Some(album) = database
        .album_row_by_media_uri(album_uri, cancellation)
        .await?
    else {
        return Ok(None);
    };
    let page = database
        .query_track_route_page(
            &library::TrackQuery {
                source: album.source_key,
                collection: Some(library::QueueCollection::AlbumKey(album.album_key)),
                folder: None,
                favorites_only: false,
            },
            "",
            settings.sort_key.track_sort(),
            settings.descending,
            window,
            cancellation,
        )
        .await?;
    Ok(Some((album, page)))
}
