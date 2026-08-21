use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};

use ::library::{AlbumDetail, AlbumId, AlbumSummary, FavoriteItemId, Library, MusicFolderId};
use adw::prelude::*;
use artwork::ArtworkBinding;

use crate::favorites::{
    album_favorite_key, favorite_button_is_active, favorite_icon_button, set_favorite_button_active,
};
use crate::format_duration_units;
use crate::localization::bind_label_text_with;
use crate::shell::Shell;
use crate::shell::actions::{ActionButtonVariant, configure_action_button};
use crate::shell::route::{LatestMountedRouteRead, MountedRoute, SelectedRouteIdentity};
use crate::{LibraryListKey, LibraryListSettings};
use ::library::RadioSeed;
use localization::{msgid, tr, track_count_text};
use playback::RadioPlayRequest;

use super::collection_context::present_album_context_menu;
use super::collections::CollectionPlay;
use super::collections::{library_route_inset, set_library_table_content_height};
use super::detail_links::{DetailLinkBinding, album_artist_links};
use super::detail_showcase::{
    DetailExternalLinksProjection, DetailSummaryProjection, MediaDetailShowcase,
    album_external_links, detail_action_row, detail_cover_projection, detail_genre_pill_button,
    detail_playback_controls, detail_radio_button, fit_detail_text, fitted_detail_title_label,
    media_detail_showcase,
};
use super::library_fields::nonzero_year;
use super::release_kind::album_release_kind_label;
use super::route::Route;
use super::route_layout::{
    PRIMARY_ROUTE_MARGIN_END, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
    detail_route_inner_width, detail_route_scroller, detail_route_wrapper,
    detail_showcase_cover_size,
};
use super::routes::SearchableTrackOptions;
use super::track_model::{
    PreparedTrackProjection, TrackProjectionRequest, prepare_track_projection,
};

const ALBUM_DETAIL_ROUTE_INSET: i32 = PRIMARY_ROUTE_MARGIN_START + PRIMARY_ROUTE_MARGIN_END;

#[derive(Clone)]
struct AlbumDetailReadRequest {
    identity: SelectedRouteIdentity,
    tracks: TrackProjectionRequest,
}

struct PreparedAlbumDetail {
    summary: AlbumSummary,
    tracks: PreparedTrackProjection,
}

impl Shell {
    pub(crate) fn album_detail_view(
        self: &Rc<Self>,
        album_id: AlbumId,
        detail: Option<AlbumDetail>,
        loaded: Arc<Library>,
        music_folder_id: Option<MusicFolderId>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view("Album", msgid("This isn't available")),
            );
        };
        let album = Arc::clone(&detail.summary.album);
        let tracks = detail.tracks.clone();
        let current_album = Rc::new(RefCell::new(detail.summary.clone()));
        let context_id = format!("album:{}", album_id.as_str());
        let applied_external_link_settings = Rc::new(RefCell::new(
            self.settings.current.borrow().external_site_links.clone(),
        ));

        let wrapper = detail_route_wrapper(0);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 22);
        content.set_margin_top(ROUTE_TOP_MARGIN);
        content.set_margin_bottom(36);
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
            LibraryListKey::AlbumDetailTracks,
            SearchableTrackOptions {
                on_visible_count_changed: Some(resize_tracks),
                context_id: context_id.clone(),
                content_inset: ALBUM_DETAIL_ROUTE_INSET,
                fixed_layout: None,
            },
        );
        let cover_size = detail_showcase_cover_size(inner_content_width);
        let cover = detail_cover_projection(
            self,
            ArtworkBinding::album_artwork(&detail.summary.artwork),
            cover_size,
            "album-detail-cover",
        );
        let facts = DetailSummaryProjection::new(&album_summary_items(&detail.summary));
        let track_count = Rc::new(Cell::new(detail.summary.track_count));
        let localized_track_count = Rc::clone(&track_count);
        facts.bind_text_with(1, move || {
            track_count_text(u64::from(localized_track_count.get()))
        });

        let text_stack = gtk::Box::new(gtk::Orientation::Vertical, 8);
        text_stack.set_hexpand(true);
        text_stack.set_halign(gtk::Align::Fill);
        text_stack.set_width_request(1);
        let kind_message = Rc::new(RefCell::new(album_release_kind_label(&album)));
        let kind = gtk::Label::new(None);
        let localized_kind = Rc::clone(&kind_message);
        bind_label_text_with(&kind, move || tr(*localized_kind.borrow()));
        kind.add_css_class("eyebrow");
        kind.set_xalign(0.0);
        kind.set_halign(gtk::Align::Start);
        kind.set_valign(gtk::Align::Center);
        kind.set_margin_end(6);
        let kind_row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        kind_row.add_css_class("album-detail-kind-row");
        kind_row.add_css_class("album-detail-genre-row");
        kind_row.set_valign(gtk::Align::Center);
        kind_row.set_halign(gtk::Align::Start);
        kind_row.append(&kind);

        let radio = detail_radio_button();
        let radio_controller = self.products.playback.radio.clone();
        let radio_album = Rc::clone(&current_album);
        radio.connect_clicked(move |_| {
            radio_controller.play_radio(RadioPlayRequest::now(RadioSeed::Album(
                radio_album.borrow().album.id.clone(),
            )));
        });
        kind_row.append(&radio);
        let genres = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        kind_row.append(&genres);
        self.append_album_genre_buttons(&genres, &album.relations.genres);

        let title = fitted_detail_title_label(&album.title);
        let artist = gtk::Label::new(Some(&album.artist));
        artist.add_css_class("detail-artist");
        artist.set_xalign(0.0);
        artist.set_halign(gtk::Align::Start);
        artist.set_wrap(true);
        artist.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        artist.set_width_request(1);
        artist.set_width_chars(1);
        artist.set_max_width_chars(32);
        fit_detail_text(&artist, &album.artist);
        let artist_links = DetailLinkBinding::new(&artist, self);
        artist_links.bind(album_artist_links(&album));
        text_stack.append(&kind_row);
        text_stack.append(&title);
        text_stack.append(&artist);
        text_stack.append(&facts.widget());

        let actions = detail_action_row();
        actions.add_css_class("album-detail-actions");
        actions.set_halign(gtk::Align::Start);
        let play_controller = self.products.playback.queue.clone();
        let play_tracks = track_projection.clone();
        let play_context_id = context_id.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled_start| {
            if let Some(request) =
                play_tracks.source_play_request(placement, &play_context_id, shuffled_start)
            {
                play_controller.play_loaded(request);
            }
        });
        let cover_controls = detail_playback_controls(
            &actions,
            msgid("Play album"),
            Some(album.favorite),
            true,
            Rc::clone(&play),
        );

        let favorite = favorite_icon_button("Favorite");
        configure_action_button(&favorite, ActionButtonVariant::DetailFavorite, None);
        set_favorite_button_active(&favorite, album.favorite);
        actions.append(&favorite);
        let hover_favorite = cover_controls
            .favorite
            .as_ref()
            .expect("album detail has a Favorite cover control")
            .clone();
        for button in [favorite, hover_favorite] {
            self.register_favorite_button(album_favorite_key(&album.id), &button);
            let shell = Rc::clone(self);
            let favorite_album_id = album.id.clone();
            button.connect_clicked(move |button| {
                shell.set_favorite_with_feedback(
                    FavoriteItemId::Album(favorite_album_id.clone()),
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

        let external_links = DetailExternalLinksProjection::new(
            Some("album-detail-link-stack"),
            album_external_links(self, &album),
        );
        let showcase = media_detail_showcase(
            self,
            MediaDetailShowcase {
                route_class: "album-detail-showcase",
                seed: album.color_seed,
                initial_width: inner_content_width,
                cover: cover.clone(),
                cover_controls,
                context_menu: Some(context_menu),
                external_links: external_links.clone(),
                text_stack: text_stack.upcast(),
                actions: actions.upcast(),
            },
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
        self.set_route_search(Some(track_projection.search()));
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

        let identity = self.mounted_route_read_identity(
            Route::AlbumDetail(album_id.clone()),
            &loaded,
            music_folder_id.clone(),
        );
        let apply = {
            let shell = Rc::clone(self);
            let album_id = album_id.clone();
            let route_stack = route_stack.clone();
            let current_album = Rc::clone(&current_album);
            let cover = cover.clone();
            let facts = facts.clone();
            let track_count = Rc::clone(&track_count);
            let kind_message = Rc::clone(&kind_message);
            let kind = kind.clone();
            let genres = genres.clone();
            let title = title.clone();
            let artist = artist.clone();
            let artist_links = artist_links.clone();
            let external_links = external_links.clone();
            let track_projection = track_projection.clone();
            Rc::new(
                move |request: AlbumDetailReadRequest,
                      result: Result<Option<PreparedAlbumDetail>, String>| {
                    if !shell.mounted_route_read_is_current(&request.identity) {
                        return;
                    }
                    let next = match result {
                        Ok(next) => next,
                        Err(error) => {
                            tracing::warn!(%error, "failed to read the mounted Album route");
                            return;
                        }
                    };
                    let Some(next) = next else {
                        route_stack.set_visible_child_name("missing");
                        return;
                    };
                    if !track_projection.replace_prepared(next.tracks) {
                        return;
                    }
                    let album = Arc::clone(&next.summary.album);
                    debug_assert_eq!(album.id, album_id);
                    cover.replace(&shell, ArtworkBinding::album_artwork(&next.summary.artwork));
                    facts.replace(&album_summary_items(&next.summary));
                    track_count.set(next.summary.track_count);
                    let next_kind = album_release_kind_label(&album);
                    kind_message.replace(next_kind);
                    kind.set_text(&tr(next_kind));
                    title.set_text(&album.title);
                    fit_detail_text(&title, &album.title);
                    artist_links.bind(album_artist_links(&album));
                    fit_detail_text(&artist, &album.artist);
                    while let Some(child) = genres.first_child() {
                        genres.remove(&child);
                    }
                    shell.append_album_genre_buttons(&genres, &album.relations.genres);
                    shell.update_visible_favorite_buttons(
                        &FavoriteItemId::Album(album.id.clone()),
                        album.favorite,
                    );
                    external_links.replace(album_external_links(&shell, &album));
                    current_album.replace(next.summary);
                    route_stack.set_visible_child_name("content");
                },
            )
        };
        let load = {
            let loaded = Arc::clone(&loaded);
            let album_id = album_id.clone();
            let music_folder_id = music_folder_id.clone();
            Arc::new(move |request: &AlbumDetailReadRequest| {
                load_album_detail(
                    &loaded,
                    &album_id,
                    music_folder_id.as_ref(),
                    &request.tracks.settings,
                )
                .and_then(|detail| {
                    detail
                        .map(|detail| {
                            prepare_track_projection(detail.tracks, request.tracks.clone())
                                .map(|tracks| PreparedAlbumDetail {
                                    summary: detail.summary,
                                    tracks,
                                })
                                .map_err(|error| error.to_string())
                        })
                        .transpose()
                })
            })
        };
        let read = LatestMountedRouteRead::new_with_request(apply, load, "mounted Album route");
        {
            let read = Rc::downgrade(&read);
            let identity = identity.clone();
            track_projection.connect_search_request(move |tracks| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with_if_running(AlbumDetailReadRequest {
                    identity: identity.clone(),
                    tracks,
                });
            });
        }
        let resume = {
            let shell = Rc::clone(self);
            let album = Rc::clone(&current_album);
            let external_links = external_links.clone();
            let applied_external_link_settings = Rc::clone(&applied_external_link_settings);
            let track_projection = track_projection.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move || {
                let external_link_settings =
                    shell.settings.current.borrow().external_site_links.clone();
                if *applied_external_link_settings.borrow() != external_link_settings {
                    external_links.replace(album_external_links(&shell, &album.borrow().album));
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
                read.request_with_if_running(AlbumDetailReadRequest {
                    identity: identity.clone(),
                    tracks: track_projection.projection_request(),
                });
            })
        };
        let update = {
            let album_id = album_id.clone();
            let track_projection = track_projection.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            let music_folder_id = music_folder_id.clone();
            Rc::new(move |update: &crate::runtime::SelectedLibraryUpdate| {
                let replacements = update.change.tracks.as_slice();
                if !update.change.albums.contains(&album_id) {
                    let exact_membership = matches!(
                        replacements,
                        [replacement]
                            if replacement
                                .track
                                .as_ref()
                                .is_some_and(|track| track.album_id.is_some())
                    );
                    if replacements.is_empty()
                        || (exact_membership
                            && track_projection.apply_track_replacement(replacements, |track| {
                                track.album_id.as_ref() == Some(&album_id)
                                    && music_folder_id.as_ref().is_none_or(|folder_id| {
                                        track.relations.music_folders.contains(folder_id)
                                    })
                            }))
                    {
                        return;
                    }
                }
                read.request_with(AlbumDetailReadRequest {
                    identity: identity.clone(),
                    tracks: track_projection.projection_request(),
                });
            })
        };
        MountedRoute::new(route_stack.upcast(), resume)
            .with_item_navigation(item_navigation)
            .with_library_update(update)
    }

    fn append_album_genre_buttons(
        self: &Rc<Self>,
        row: &gtk::Box,
        genres: &[::library::GenreCredit],
    ) {
        for genre in genres.iter().filter(|genre| !genre.name.trim().is_empty()) {
            let button = detail_genre_pill_button(genre.name.trim());
            let shell = Rc::clone(self);
            let genre_id = genre.id.clone();
            button.connect_clicked(move |_| shell.navigate(Route::GenreDetail(genre_id.clone())));
            row.append(&button);
        }
    }
}

pub(crate) fn load_album_detail(
    loaded: &Arc<Library>,
    album_id: &AlbumId,
    music_folder_id: Option<&MusicFolderId>,
    settings: &LibraryListSettings,
) -> Result<Option<AlbumDetail>, String> {
    let mut detail = loaded
        .album_detail(album_id, music_folder_id)
        .map_err(|error| error.to_string())?;
    if let Some(detail) = detail.as_mut() {
        detail.tracks = detail
            .tracks
            .sorted(settings.sort_key.track_sort(), settings.descending)
            .map_err(|error| error.to_string())?;
    }
    Ok(detail)
}

fn album_summary_items(summary: &AlbumSummary) -> Vec<(&'static str, String)> {
    vec![
        (
            "rufin-x-office-calendar-symbolic",
            nonzero_year(summary.album.year),
        ),
        (
            "rufin-tracks-symbolic",
            track_count_text(summary.track_count.into()),
        ),
        (
            "rufin-preferences-system-time-symbolic",
            format_duration_units(summary.duration_seconds),
        ),
    ]
}
