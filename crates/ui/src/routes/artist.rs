use std::{cell::RefCell, rc::Rc, sync::Arc};

use adw::prelude::*;
use artwork::ArtworkBinding;
use library::{ArtistKey, ArtistRow, Database, RadioSeed, ReadCancellation};
use localization::{album_count_text, msgid, track_count_text};
use playback::RadioPlayRequest;

use crate::favorites::{
    artist_favorite_key, favorite_button_is_active, favorite_icon_button,
    set_favorite_button_active,
};
use crate::layout::width_allocation_owner;
use crate::localization::localized_label;
use crate::shell::Shell;
use crate::shell::actions::{ActionButtonVariant, configure_action_button};
use crate::shell::cover::presentation::stable_seed;
use crate::shell::route::MountedRoute;
use crate::{LibraryListKey, LibraryListSettings};

use super::artist_releases::{ArtistReleaseProjections, ArtistReleaseRoutePreamble};
use super::collection_context::present_artist_context_menu;
use super::collections::{CollectionPlay, PlaybackTarget};
use super::detail_showcase::{
    DetailCoverProjection, DetailExternalLinksProjection, MediaDetailShowcase, detail_action_row,
    detail_cover_projection, detail_playback_controls, detail_radio_button,
    fitted_detail_title_label, media_detail_showcase,
};
use super::route::Route;
use super::route_layout::{PRIMARY_ROUTE_MARGIN_START, detail_route_inner_width};
use super::routes::SearchableTrackOptions;
use super::track_model::{PreparedTrackProjection, TrackProjectionRequest};

#[derive(Clone)]
pub(crate) struct ArtistOverviewData {
    pub(crate) summary: ArtistRow,
    pub(crate) favorite_tracks: Vec<library::TrackKey>,
    pub(crate) favorite_first_rows: Vec<library::TrackRow>,
    pub(crate) releases: ArtistReleaseOrders,
}

#[derive(Clone)]
pub(crate) struct ArtistTracksData {
    pub(crate) summary: ArtistRow,
    pub(crate) tracks: Vec<library::TrackKey>,
    pub(crate) first_rows: Vec<library::TrackRow>,
}

#[derive(Clone)]
pub(crate) struct ArtistDiscographyData {
    pub(crate) summary: ArtistRow,
    pub(crate) releases: ArtistReleaseOrders,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ArtistReleaseOrders {
    sections: [Vec<library::AlbumKey>; 6],
    first_rows: Vec<library::AlbumRow>,
}

const ARTIST_RELEASE_TITLES: [&str; 6] = [
    msgid("Albums"),
    msgid("EPs"),
    msgid("Singles"),
    msgid("Collections"),
    msgid("Other releases"),
    msgid("Appears On"),
];

pub(crate) fn artist_detail_route(artist: ArtistKey, album_artist: bool) -> Route {
    if album_artist {
        Route::AlbumArtistDetail(artist)
    } else {
        Route::ArtistDetail(artist)
    }
}

fn artist_discography_route(artist: ArtistKey, album_artist: bool) -> Route {
    if album_artist {
        Route::AlbumArtistDiscography(artist)
    } else {
        Route::ArtistDiscography(artist)
    }
}

fn artist_tracks_route(artist: ArtistKey, album_artist: bool) -> Route {
    if album_artist {
        Route::AlbumArtistTracks(artist)
    } else {
        Route::ArtistTracks(artist)
    }
}

fn artist_favorite_tracks_route(artist: ArtistKey, album_artist: bool) -> Route {
    if album_artist {
        Route::AlbumArtistFavoriteTracks(artist)
    } else {
        Route::ArtistFavoriteTracks(artist)
    }
}

fn artist_playback_target(artist: ArtistKey, album_artist: bool) -> PlaybackTarget {
    if album_artist {
        PlaybackTarget::AlbumArtist(artist)
    } else {
        PlaybackTarget::Artist(artist)
    }
}

fn apply_artist_primary_search(shell: &Rc<Shell>, releases: &ArtistReleaseProjections) {
    if let Some(target) = releases.primary_search() {
        shell.set_route_search_with_focus(target.search, target.focus);
    } else {
        shell.set_route_search(None);
    }
}

impl Shell {
    pub(crate) fn artist_detail_view(
        self: &Rc<Self>,
        artist: ArtistKey,
        album_artist: bool,
        detail: Option<ArtistOverviewData>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view("Artist", msgid("No music from this artist yet")),
            );
        };
        let summary = detail.summary.clone();
        let header = artist_detail_header_restored(self, &summary, artist, album_artist);
        let favorite_count = detail.favorite_tracks.len();
        let favorite = self.searchable_track_collection(
            &selected,
            detail.favorite_tracks,
            detail.favorite_first_rows,
            LibraryListKey::ArtistTracks,
            SearchableTrackOptions {
                on_visible_count_changed: None,
                context_id: format!(
                    "{}:{artist}|favorites",
                    if album_artist {
                        "album-artist"
                    } else {
                        "artist"
                    }
                ),
                content_inset: 0,
                fixed_layout: Some(crate::LibraryLayout::Row),
                search: None,
            },
        );
        let favorite_present = !favorite.source_is_empty();
        let favorite_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        favorite_section.set_visible(favorite_present);
        let heading = localized_label(msgid("Favorite tracks"));
        heading.add_css_class("section-heading");
        heading.set_xalign(0.0);
        favorite_section.append(&heading);
        let favorite_toolbar =
            self.library_toolbar_projection(LibraryListKey::ArtistTracks, favorite.search());
        favorite_toolbar.set_layout_control_visible(false);
        favorite_section.append(&favorite_toolbar.widget());
        let favorite_scroller = gtk::ScrolledWindow::new();
        favorite_scroller.add_css_class("non-propagating-width-clip");
        crate::layout::configure_fill_width_clip(&favorite_scroller, gtk::PolicyType::Automatic);
        favorite_scroller.set_overlay_scrolling(true);
        favorite_scroller.set_width_request(1);
        let height = 30 + favorite_count.min(4) as i32 * 64;
        favorite_scroller.set_min_content_height(height);
        favorite_scroller.set_max_content_height(height);
        favorite_scroller.set_propagate_natural_height(false);
        favorite_section.append(&favorite.mount_in_scroller(&favorite_scroller));
        let ArtistReleaseOrders {
            sections,
            first_rows,
        } = detail.releases;
        let releases = Rc::new(ArtistReleaseProjections::new(
            self,
            &selected,
            ArtistReleaseRoutePreamble {
                header: header.widget(),
                favorite: Some((favorite_section.clone().upcast(), favorite.search())),
                favorite_present,
                empty: self.route_empty_view(msgid("No music from this artist yet")),
            },
            ARTIST_RELEASE_TITLES,
            sections,
            first_rows,
        ));
        let refresh_primary_search: Rc<dyn Fn()> = {
            let shell = Rc::downgrade(self);
            let releases = Rc::downgrade(&releases);
            Rc::new(move || {
                if let (Some(shell), Some(releases)) = (shell.upgrade(), releases.upgrade()) {
                    apply_artist_primary_search(&shell, &releases);
                }
            })
        };
        connect_artist_release_requests(
            self,
            &selected,
            artist,
            album_artist,
            &releases,
            Rc::clone(&refresh_primary_search),
        );
        refresh_primary_search();
        let resume_releases = Rc::clone(&releases);
        let resume_shell = Rc::downgrade(self);
        let resume_selected = selected.clone();
        let resume_primary_search = Rc::clone(&refresh_primary_search);
        let resume_lane = releases.lane();
        let resume = Rc::new(move || {
            let Some(resume_shell) = resume_shell.upgrade() else {
                return;
            };
            let settings = resume_shell
                .settings
                .current
                .borrow()
                .library_list(LibraryListKey::ArtistAlbums);
            request_artist_release_orders(
                Rc::downgrade(&resume_shell),
                resume_selected.clone(),
                artist,
                album_artist,
                Rc::clone(&resume_releases),
                settings,
                Rc::clone(&resume_primary_search),
                Rc::clone(&resume_lane),
            );
        });
        let refresh_lane = releases.lane();
        let refresh = {
            let shell = Rc::downgrade(self);
            let selected = selected.clone();
            let favorite = favorite.clone();
            let favorite_scroller = favorite_scroller.clone();
            let releases = Rc::clone(&releases);
            let header = header.clone();
            let refresh_primary_search = Rc::clone(&refresh_primary_search);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let track_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                let album_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                let request = favorite.projection_request();
                let (generation, cancellation) = refresh_lane.begin();
                let database = Arc::clone(&selected.database);
                let selected_task = selected.clone();
                let task_album_settings = album_settings.clone();
                let task = selected.runtime.spawn(async move {
                    load_artist_overview(
                        &database,
                        selected_task.source_key,
                        selected_task.music_folder_key,
                        artist,
                        album_artist,
                        &track_settings,
                        &task_album_settings,
                        &cancellation,
                    )
                    .await
                });
                let favorite = favorite.clone();
                let favorite_scroller = favorite_scroller.clone();
                let releases = releases.clone();
                let header = header.clone();
                let lane = Rc::clone(&refresh_lane);
                let shell_apply = Rc::clone(&shell);
                let refresh_primary_search = Rc::clone(&refresh_primary_search);
                gtk::glib::spawn_future_local(async move {
                    let Ok(Ok(Some(detail))) = task.await else {
                        return;
                    };
                    if !lane.finish(generation) {
                        return;
                    }
                    header.replace(&shell_apply, detail.summary.clone());
                    favorite.replace_prepared(PreparedTrackProjection {
                        order: detail.favorite_tracks,
                        first_rows: detail.favorite_first_rows,
                        request,
                    });
                    let present = !favorite.source_is_empty();
                    releases.set_favorite_present(present);
                    let height = 30 + favorite.source_count().min(4) as i32 * 64;
                    favorite_scroller.set_min_content_height(height);
                    favorite_scroller.set_max_content_height(height);
                    releases.replace_orders(
                        detail.releases.sections,
                        detail.releases.first_rows,
                        true,
                    );
                    releases.apply_library_list_settings(&album_settings);
                    refresh_primary_search();
                });
            }) as Rc<dyn Fn()>
        };
        MountedRoute::new(releases.widget(), resume).with_catalog_refresh(refresh)
    }

    pub(crate) fn artist_tracks_view(
        self: &Rc<Self>,
        artist: ArtistKey,
        album_artist: bool,
        detail: Option<ArtistTracksData>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Tracks"), msgid("This isn't available")),
            );
        };
        self.artist_track_surface_restored(
            artist,
            artist_tracks_route(artist, album_artist),
            detail.summary,
            detail.tracks,
            detail.first_rows,
            selected,
            album_artist,
            false,
        )
    }

    pub(crate) fn artist_favorite_tracks_view(
        self: &Rc<Self>,
        artist: ArtistKey,
        album_artist: bool,
        detail: Option<ArtistTracksData>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Favorite tracks"), msgid("No favorites yet")),
            );
        };
        self.artist_track_surface_restored(
            artist,
            artist_favorite_tracks_route(artist, album_artist),
            detail.summary,
            detail.tracks,
            detail.first_rows,
            selected,
            album_artist,
            true,
        )
    }

    pub(crate) fn artist_discography_view(
        self: &Rc<Self>,
        artist: ArtistKey,
        album_artist: bool,
        detail: Option<ArtistDiscographyData>,
        selected: crate::runtime::SelectedLibrary,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(self.placeholder_view(
                msgid("Discography"),
                msgid("No albums from this artist yet"),
            ));
        };
        let ArtistReleaseOrders {
            sections,
            first_rows,
        } = detail.releases;
        let releases = Rc::new(ArtistReleaseProjections::new(
            self,
            &selected,
            ArtistReleaseRoutePreamble {
                header: artist_subroute_header(self, &detail.summary, msgid("Discography")),
                favorite: None,
                favorite_present: false,
                empty: self.route_empty_view(msgid("No albums from this artist yet")),
            },
            ARTIST_RELEASE_TITLES,
            sections,
            first_rows,
        ));
        let refresh_primary_search: Rc<dyn Fn()> = {
            let shell = Rc::downgrade(self);
            let releases = Rc::downgrade(&releases);
            Rc::new(move || {
                if let (Some(shell), Some(releases)) = (shell.upgrade(), releases.upgrade()) {
                    apply_artist_primary_search(&shell, &releases);
                }
            })
        };
        connect_artist_release_requests(
            self,
            &selected,
            artist,
            album_artist,
            &releases,
            Rc::clone(&refresh_primary_search),
        );
        refresh_primary_search();
        let resume_releases = Rc::clone(&releases);
        let resume_shell = Rc::downgrade(self);
        let resume_selected = selected.clone();
        let resume_primary_search = Rc::clone(&refresh_primary_search);
        let resume_lane = releases.lane();
        let resume = Rc::new(move || {
            let Some(resume_shell) = resume_shell.upgrade() else {
                return;
            };
            let settings = resume_shell
                .settings
                .current
                .borrow()
                .library_list(LibraryListKey::ArtistAlbums);
            request_artist_release_orders(
                Rc::downgrade(&resume_shell),
                resume_selected.clone(),
                artist,
                album_artist,
                Rc::clone(&resume_releases),
                settings,
                Rc::clone(&resume_primary_search),
                Rc::clone(&resume_lane),
            );
        });
        let refresh = {
            let selected = selected.clone();
            let releases = Rc::clone(&releases);
            let shell = Rc::downgrade(self);
            let refresh_primary_search = Rc::clone(&refresh_primary_search);
            let refresh_lane = releases.lane();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                request_artist_release_orders(
                    Rc::downgrade(&shell),
                    selected.clone(),
                    artist,
                    album_artist,
                    Rc::clone(&releases),
                    settings,
                    Rc::clone(&refresh_primary_search),
                    Rc::clone(&refresh_lane),
                );
            }) as Rc<dyn Fn()>
        };
        MountedRoute::new(releases.widget(), resume).with_catalog_refresh(refresh)
    }

    fn artist_track_surface_restored(
        self: &Rc<Self>,
        artist: ArtistKey,
        route: Route,
        summary: ArtistRow,
        order: Vec<library::TrackKey>,
        first_rows: Vec<library::TrackRow>,
        selected: crate::runtime::SelectedLibrary,
        album_artist: bool,
        favorites_only: bool,
    ) -> MountedRoute {
        let key = LibraryListKey::ArtistTracks;
        let context = if favorites_only && album_artist {
            "album-artist-favorite-tracks"
        } else if favorites_only {
            "artist-favorite-tracks"
        } else if album_artist {
            "album-artist-tracks"
        } else {
            "artist-tracks"
        };
        let (tracks_widget, tracks, toolbar) = self.scrolling_track_projection(
            &selected,
            order,
            first_rows,
            key,
            context,
            format!(
                "{}:{artist}",
                if album_artist {
                    "album-artist"
                } else {
                    "artist"
                }
            ),
        );
        let tracks = Rc::new(tracks);
        let root = super::route_layout::detail_route_wrapper(18);
        root.append(&artist_subroute_header(
            self,
            &summary,
            if favorites_only {
                msgid("Favorite tracks")
            } else {
                msgid("Tracks")
            },
        ));
        root.append(&tracks_widget);
        let lane = Rc::new(super::named_detail::NamedOrderLane::new());
        {
            let shell = Rc::downgrade(self);
            let projection = Rc::downgrade(&tracks);
            let selected = selected.clone();
            let route = route.clone();
            let lane = Rc::clone(&lane);
            tracks.connect_search_request(move |request| {
                request_artist_order(
                    shell.clone(),
                    projection.clone(),
                    selected.clone(),
                    artist,
                    route.clone(),
                    request,
                    album_artist,
                    favorites_only,
                    Rc::clone(&lane),
                );
            });
        }
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = Rc::clone(&tracks);
            let selected = selected.clone();
            let lane = Rc::clone(&lane);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                projection.apply_library_list_settings(key, &settings);
                toolbar.apply(key, &settings);
                request_artist_order(
                    Rc::downgrade(&shell),
                    Rc::downgrade(&projection),
                    selected.clone(),
                    artist,
                    route.clone(),
                    projection.projection_request(),
                    album_artist,
                    favorites_only,
                    Rc::clone(&lane),
                );
            })
        };
        MountedRoute::new(root.upcast(), resume).with_item_navigation(tracks.item_navigation())
    }
}

fn request_artist_order(
    shell: std::rc::Weak<Shell>,
    projection: std::rc::Weak<super::routes::TrackListProjection>,
    selected: crate::runtime::SelectedLibrary,
    artist: ArtistKey,
    route: Route,
    request: TrackProjectionRequest,
    album_artist: bool,
    favorites_only: bool,
    lane: Rc<super::named_detail::NamedOrderLane>,
) {
    let (generation, cancellation) = lane.begin();
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    let query = request.query.clone();
    let sort = request.settings.sort_key.track_sort();
    let descending = request.settings.descending;
    let task = selected.runtime.spawn(async move {
        database
            .artist_track_route_page(
                source,
                artist,
                album_artist,
                folder,
                &query,
                sort,
                descending,
                favorites_only,
                &cancellation,
            )
            .await
    });
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else { return };
        let page = task.await.ok().and_then(Result::ok);
        let Some(projection) = projection.upgrade() else {
            return;
        };
        if !lane.finish(generation) || shell.navigation.routes.borrow().current() != &route {
            return;
        }
        if let Some(page) = page {
            projection.replace_prepared(PreparedTrackProjection {
                order: page.order,
                first_rows: page.first_rows,
                request,
            });
        }
    });
}

fn connect_artist_release_requests(
    shell: &Rc<Shell>,
    selected: &crate::runtime::SelectedLibrary,
    artist: ArtistKey,
    album_artist: bool,
    releases: &Rc<ArtistReleaseProjections>,
    refresh_primary_search: Rc<dyn Fn()>,
) {
    for index in 0..ARTIST_RELEASE_TITLES.len() {
        let Some(search) = releases.section_search(index) else {
            continue;
        };
        let weak_shell = Rc::downgrade(shell);
        let selected = selected.clone();
        let releases = Rc::downgrade(releases);
        let refresh_primary_search = Rc::clone(&refresh_primary_search);
        search.connect_search_changed(move |_| {
            let Some(shell) = weak_shell.upgrade() else {
                return;
            };
            let Some(releases) = releases.upgrade() else {
                return;
            };
            let settings = shell
                .settings
                .current
                .borrow()
                .library_list(LibraryListKey::ArtistAlbums);
            request_artist_release_section(
                Rc::downgrade(&shell),
                selected.clone(),
                artist,
                album_artist,
                releases,
                index,
                settings,
                Rc::clone(&refresh_primary_search),
            );
        });
    }
}

fn request_artist_release_orders(
    shell: std::rc::Weak<Shell>,
    selected: crate::runtime::SelectedLibrary,
    artist: ArtistKey,
    album_artist: bool,
    releases: Rc<ArtistReleaseProjections>,
    settings: LibraryListSettings,
    refresh_primary_search: Rc<dyn Fn()>,
    lane: Rc<super::named_detail::NamedOrderLane>,
) {
    releases.apply_library_list_settings(&settings);
    let (generation, cancellation) = lane.begin();
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    let task = selected.runtime.spawn(async move {
        load_artist_release_orders(
            &database,
            source,
            folder,
            artist,
            album_artist,
            "",
            &settings,
            &cancellation,
        )
        .await
    });
    let lane = Rc::downgrade(&lane);
    let releases = Rc::downgrade(&releases);
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else { return };
        let Ok(Ok(orders)) = task.await else { return };
        let (Some(lane), Some(releases)) = (lane.upgrade(), releases.upgrade()) else {
            return;
        };
        if !lane.finish(generation) {
            return;
        }
        releases.replace_orders(orders.sections, orders.first_rows, true);
        refresh_primary_search();
        shell.refresh_current_route_now_playing_selections();
    });
}

fn request_artist_release_section(
    shell: std::rc::Weak<Shell>,
    selected: crate::runtime::SelectedLibrary,
    artist: ArtistKey,
    album_artist: bool,
    releases: Rc<ArtistReleaseProjections>,
    index: usize,
    settings: LibraryListSettings,
    refresh_primary_search: Rc<dyn Fn()>,
) {
    let Some(search) = releases.section_search(index) else {
        return;
    };
    let query = search.text().trim().to_string();
    let authoritative = query.is_empty();
    let Some(lane) = releases.section_lane(index) else {
        return;
    };
    let (generation, cancellation) = lane.begin();
    let database = Arc::clone(&selected.database);
    let source = selected.source_key;
    let folder = selected.music_folder_key;
    let task = selected.runtime.spawn(async move {
        Ok::<_, library::LibraryError>(
            load_artist_release_orders(
                &database,
                source,
                folder,
                artist,
                album_artist,
                &query,
                &settings,
                &cancellation,
            )
            .await?
            .sections[index]
                .clone(),
        )
    });
    let lane = Rc::downgrade(&lane);
    let releases = Rc::downgrade(&releases);
    gtk::glib::spawn_future_local(async move {
        let Some(shell) = shell.upgrade() else { return };
        let Ok(Ok(order)) = task.await else { return };
        let (Some(lane), Some(releases)) = (lane.upgrade(), releases.upgrade()) else {
            return;
        };
        if !lane.finish(generation) {
            return;
        }
        releases.replace_section_order(index, order, authoritative);
        if authoritative {
            refresh_primary_search();
        }
        shell.refresh_current_route_now_playing_selections();
    });
}

pub(crate) async fn load_artist_overview(
    database: &Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    artist: ArtistKey,
    album_artist: bool,
    track_settings: &LibraryListSettings,
    album_settings: &LibraryListSettings,
    cancellation: &ReadCancellation,
) -> Result<Option<ArtistOverviewData>, String> {
    let Some(detail) = database
        .artist_detail(source, artist, album_artist, folder, cancellation)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let favorite_tracks = database
        .artist_track_route_page(
            source,
            artist,
            album_artist,
            folder,
            "",
            track_settings.sort_key.track_sort(),
            track_settings.descending,
            true,
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    let releases = load_artist_release_orders(
        database,
        source,
        folder,
        artist,
        album_artist,
        "",
        album_settings,
        cancellation,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(Some(ArtistOverviewData {
        summary: detail.artist,
        favorite_tracks: favorite_tracks.order,
        favorite_first_rows: favorite_tracks.first_rows,
        releases,
    }))
}

pub(crate) async fn load_artist_discography(
    database: &Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    artist: ArtistKey,
    album_artist: bool,
    settings: &LibraryListSettings,
    cancellation: &ReadCancellation,
) -> Result<Option<ArtistDiscographyData>, String> {
    let Some(detail) = database
        .artist_detail(source, artist, album_artist, folder, cancellation)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let releases = load_artist_release_orders(
        database,
        source,
        folder,
        artist,
        album_artist,
        "",
        settings,
        cancellation,
    )
    .await
    .map_err(|error| error.to_string())?;
    Ok(Some(ArtistDiscographyData {
        summary: detail.artist,
        releases,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn load_artist_release_orders(
    database: &Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    artist: ArtistKey,
    album_artist: bool,
    query: &str,
    settings: &LibraryListSettings,
    cancellation: &ReadCancellation,
) -> library::LibraryResult<ArtistReleaseOrders> {
    let albums = database
        .artist_album_projection_order(
            source,
            artist,
            album_artist,
            folder,
            query,
            settings.sort_key.album_sort(),
            settings.descending,
            cancellation,
        )
        .await?;
    let classifications = database
        .album_release_classifications(source, &albums, cancellation)
        .await?;
    let mut releases = partition_artist_releases(classifications, Vec::new());
    let first = releases
        .sections
        .iter()
        .flatten()
        .copied()
        .take(64)
        .collect::<Vec<_>>();
    releases.first_rows = database
        .album_rows(source, &first, folder, cancellation)
        .await?;
    Ok(releases)
}

fn partition_artist_releases(
    albums: Vec<library::AlbumReleaseClassification>,
    appears_on: Vec<library::AlbumKey>,
) -> ArtistReleaseOrders {
    let mut sections: [Vec<library::AlbumKey>; 6] = std::array::from_fn(|_| Vec::new());
    for album in albums {
        let index = match album.class {
            library::AlbumReleaseClass::Album => 0,
            library::AlbumReleaseClass::Ep => 1,
            library::AlbumReleaseClass::Single => 2,
            library::AlbumReleaseClass::Collection => 3,
            library::AlbumReleaseClass::Other => 4,
        };
        sections[index].push(album.album_key);
    }
    sections[5] = appears_on;
    ArtistReleaseOrders {
        sections,
        first_rows: Vec::new(),
    }
}

pub(crate) async fn load_artist_tracks(
    database: &Database,
    source: library::SourceKey,
    folder: Option<library::FolderKey>,
    artist: ArtistKey,
    album_artist: bool,
    settings: &LibraryListSettings,
    favorites_only: bool,
    cancellation: &ReadCancellation,
) -> Result<Option<ArtistTracksData>, String> {
    let Some(detail) = database
        .artist_detail(source, artist, album_artist, folder, cancellation)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let tracks = database
        .artist_track_route_page(
            source,
            artist,
            album_artist,
            folder,
            "",
            settings.sort_key.track_sort(),
            settings.descending,
            favorites_only,
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(ArtistTracksData {
        summary: detail.artist,
        tracks: tracks.order,
        first_rows: tracks.first_rows,
    }))
}

#[derive(Clone)]
struct ArtistDetailHeaderProjection {
    root: gtk::Widget,
    current: Rc<RefCell<ArtistRow>>,
    title: gtk::Label,
    album_count: gtk::Label,
    track_count: gtk::Label,
    cover: DetailCoverProjection,
    external_links: DetailExternalLinksProjection,
}

impl ArtistDetailHeaderProjection {
    fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    fn replace(&self, shell: &Rc<Shell>, artist: ArtistRow) {
        self.title.set_text(&artist.name);
        super::detail_showcase::fit_detail_text(&self.title, &artist.name);
        self.album_count
            .set_text(&album_count_text(artist.album_count.max(0) as u64));
        self.track_count
            .set_text(&track_count_text(artist.track_count.max(0) as u64));
        self.cover.replace(
            shell,
            artist
                .artwork_binding
                .as_deref()
                .map(ArtworkBinding::opaque)
                .unwrap_or_default(),
        );
        self.external_links
            .replace(super::detail_showcase::artist_external_links(
                shell, &artist,
            ));
        shell.update_visible_favorite_buttons(
            &library::FavoriteTarget::Artist(artist.artist_key),
            artist.favorite,
        );
        self.current.replace(artist);
    }
}

fn artist_detail_header_restored(
    shell: &Rc<Shell>,
    artist: &ArtistRow,
    artist_key: ArtistKey,
    album_artist: bool,
) -> ArtistDetailHeaderProjection {
    let current = Rc::new(RefCell::new(artist.clone()));
    let width = detail_route_inner_width(shell, PRIMARY_ROUTE_MARGIN_START);
    let cover = detail_cover_projection(
        shell,
        artist
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        super::route_layout::detail_showcase_cover_size(width),
        "artist-detail-cover",
    );
    let counts = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    counts.add_css_class("artist-count-row");
    counts.set_halign(gtk::Align::Start);
    let (albums, album_count) = artist_count_button(
        "rufin-albums-symbolic",
        &album_count_text(artist.album_count.max(0) as u64),
    );
    let album_shell = Rc::clone(shell);
    albums.connect_clicked(move |_| {
        album_shell.navigate(artist_discography_route(artist_key, album_artist))
    });
    counts.append(&albums);
    let (tracks, track_count) = artist_count_button(
        "rufin-tracks-symbolic",
        &track_count_text(artist.track_count.max(0) as u64),
    );
    let track_shell = Rc::clone(shell);
    tracks.connect_clicked(move |_| {
        track_shell.navigate(artist_tracks_route(artist_key, album_artist))
    });
    counts.append(&tracks);

    let text_stack = gtk::Box::new(gtk::Orientation::Vertical, 8);
    text_stack.set_hexpand(true);
    text_stack.set_width_request(1);
    text_stack.append(&artist_kind_row(shell, artist, album_artist));
    let title = fitted_detail_title_label(&artist.name);
    text_stack.append(&title);
    text_stack.append(&counts);

    let actions = detail_action_row();
    actions.add_css_class("artist-detail-actions");
    actions.set_halign(gtk::Align::Start);
    let target = artist_playback_target(artist_key, album_artist);
    let play_shell = Rc::clone(shell);
    let play: CollectionPlay = Rc::new(move |placement, shuffled| {
        target.play(&play_shell, placement, shuffled);
    });
    let controls = detail_playback_controls(
        &actions,
        msgid("Play artist"),
        Some(artist.favorite),
        true,
        Rc::clone(&play),
    );
    let favorite = favorite_icon_button("Favorite");
    configure_action_button(&favorite, ActionButtonVariant::DetailFavorite);
    set_favorite_button_active(&favorite, artist.favorite);
    actions.append(&favorite);
    let hover_favorite = controls
        .favorite
        .as_ref()
        .expect("artist detail has a Favorite cover control")
        .clone();
    for button in [favorite, hover_favorite] {
        shell.register_favorite_button(artist_favorite_key(&artist_key), &button);
        let favorite_shell = Rc::clone(shell);
        button.connect_clicked(move |button| {
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Artist(artist_key),
                !favorite_button_is_active(button),
                Some(button),
            );
        });
    }
    let menu_shell = Rc::clone(shell);
    let menu_artist = Rc::clone(&current);
    let menu_play = Rc::clone(&play);
    let context_menu = Rc::new(move |target: &gtk::Widget, position| {
        let artist = menu_artist.borrow().clone();
        present_artist_context_menu(
            target,
            &menu_shell,
            artist,
            album_artist,
            Some(Rc::clone(&menu_play)),
            position,
        );
    });
    let external_links = DetailExternalLinksProjection::new(
        None,
        super::detail_showcase::artist_external_links(shell, artist),
    );
    let root = media_detail_showcase(
        shell,
        MediaDetailShowcase {
            route_class: "artist-detail-showcase",
            seed: stable_seed(&artist.object_id),
            initial_width: width,
            cover: cover.clone(),
            cover_controls: controls,
            context_menu: Some(context_menu),
            external_links: external_links.clone(),
            text_stack: text_stack.upcast(),
            actions: actions.upcast(),
        },
    );
    ArtistDetailHeaderProjection {
        root,
        current,
        title,
        album_count,
        track_count,
        cover,
        external_links,
    }
}

fn artist_kind_row(shell: &Rc<Shell>, artist: &ArtistRow, album_artist: bool) -> gtk::Box {
    let kind = localized_label("Artist");
    kind.add_css_class("eyebrow");
    kind.set_xalign(0.0);
    kind.set_halign(gtk::Align::Start);
    kind.set_valign(gtk::Align::Center);
    kind.set_margin_end(6);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    row.add_css_class("album-detail-kind-row");
    row.add_css_class("album-detail-genre-row");
    row.set_halign(gtk::Align::Start);
    row.set_valign(gtk::Align::Center);
    row.append(&kind);
    let radio = detail_radio_button();
    let controller = shell.products.playback.radio.clone();
    let artist_key = artist.artist_key;
    radio.connect_clicked(move |_| {
        controller.play_radio(RadioPlayRequest::now(if album_artist {
            RadioSeed::AlbumArtist(artist_key)
        } else {
            RadioSeed::Artist(artist_key)
        }));
    });
    row.append(&radio);
    row
}

fn artist_subroute_header(shell: &Rc<Shell>, artist: &ArtistRow, kind_text: &str) -> gtk::Widget {
    let width = detail_route_inner_width(shell, PRIMARY_ROUTE_MARGIN_START);
    let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
    header.add_css_class("detail-showcase");
    header.add_css_class("artist-detail-showcase");
    if width < 520 {
        header.add_css_class("detail-showcase-tiny");
    }
    crate::shell::cover::presentation::add_album_seed_gradient_class(
        &header,
        stable_seed(&artist.object_id),
    );
    let kind = localized_label(kind_text);
    kind.add_css_class("eyebrow");
    kind.set_xalign(0.0);
    let title = gtk::Label::new(Some(&artist.name));
    title.add_css_class("detail-title");
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    super::detail_showcase::fit_detail_text(&title, &artist.name);
    let summary = gtk::Label::new(Some(&format!(
        "{} / {}",
        album_count_text(artist.album_count.max(0) as u64),
        track_count_text(artist.track_count.max(0) as u64)
    )));
    summary.add_css_class("muted");
    summary.set_xalign(0.0);
    header.append(&kind);
    header.append(&title);
    header.append(&summary);
    let resize = header.clone();
    let frame = super::detail_showcase::detail_showcase_frame_with_back(shell, header.upcast());
    width_allocation_owner(&frame, move |width| {
        if width < 520 {
            resize.add_css_class("detail-showcase-tiny");
        } else {
            resize.remove_css_class("detail-showcase-tiny");
        }
    })
    .upcast()
}

fn artist_count_button(icon_name: &str, text: &str) -> (gtk::Button, gtk::Label) {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("artist-count-button");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(16);
    content.append(&icon);
    let label = gtk::Label::new(Some(text));
    content.append(&label);
    button.set_child(Some(&content));
    (button, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_sections_preserve_every_album_once() {
        let albums = [
            library::AlbumReleaseClass::Album,
            library::AlbumReleaseClass::Ep,
            library::AlbumReleaseClass::Single,
            library::AlbumReleaseClass::Collection,
            library::AlbumReleaseClass::Other,
        ]
        .into_iter()
        .enumerate()
        .map(|(index, class)| library::AlbumReleaseClassification {
            album_key: library::AlbumKey::from_raw(index as i64 + 1),
            class,
        })
        .collect::<Vec<_>>();
        let appears = vec![library::AlbumKey::from_raw(6)];
        let grouped = partition_artist_releases(albums, appears);
        let flattened = grouped.sections.into_iter().flatten().collect::<Vec<_>>();
        assert_eq!(flattened.len(), 6);
        let unique = flattened
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), flattened.len());
    }

    #[test]
    fn responsive_release_rows_preserve_complete_membership() {
        let albums = (0..17).collect::<Vec<_>>();
        for columns in 1..=6 {
            let rows = albums
                .chunks(columns)
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            assert_eq!(rows, albums);
        }
    }

    #[test]
    fn artist_and_album_artist_routes_keep_distinct_role_identity() {
        let key = ArtistKey::from_raw(7);
        assert_eq!(artist_detail_route(key, false), Route::ArtistDetail(key));
        assert_eq!(
            artist_detail_route(key, true),
            Route::AlbumArtistDetail(key)
        );
        assert!(matches!(
            artist_playback_target(key, false),
            PlaybackTarget::Artist(value) if value == key
        ));
        assert!(matches!(
            artist_playback_target(key, true),
            PlaybackTarget::AlbumArtist(value) if value == key
        ));
    }
}
