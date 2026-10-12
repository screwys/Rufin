use std::{cell::RefCell, rc::Rc, sync::Arc};

use adw::prelude::*;
use artwork::ArtworkBinding;
use library::{ArtistKey, ArtistRow, Database, RadioSeed, ReadCancellation};
use localization::{album_count_text, msgid, track_count_text};
use rufin_core::playback::RadioPlayRequest;

use crate::CatalogUi;
use crate::{LibraryListKey, LibraryListSettings};
use gtk_widgets::artwork::presentation::stable_seed;
use gtk_widgets::controls::{ActionButtonVariant, configure_action_button};
use gtk_widgets::favorites::{
    artist_favorite_key, favorite_button_is_active, favorite_icon_button,
    set_favorite_button_active,
};
use gtk_widgets::layout::width_allocation_owner;
use gtk_widgets::localization::localized_label;
use gtk_widgets::mounted_route::MountedRoute;

use super::artist_releases::{ArtistReleaseProjections, ArtistReleaseRoutePreamble};
use super::detail_showcase::{
    DetailShowcaseView, MediaCoverProjection, MediaShowcase, detail_playback_controls,
    detail_radio_button, detail_showcase_frame, media_cover_projection, media_showcase,
};
use super::routes::SearchableTrackOptions;
use crate::route_layout::PRIMARY_ROUTE_MARGIN_START;
use gtk_media_menus::media_menus::present_artist_context_menu;
use gtk_widgets::route::Route;

#[derive(Clone)]
pub struct ArtistOverviewData {
    pub summary: ArtistRow,
    pub favorite_tracks: library::TrackRoutePage,
    pub releases: ArtistReleasePage,
}

#[derive(Clone)]
pub struct ArtistTracksData {
    pub summary: ArtistRow,
    pub tracks: library::TrackRoutePage,
}

#[derive(Clone)]
pub struct ArtistDiscographyData {
    pub summary: ArtistRow,
    pub releases: ArtistReleasePage,
}

use rufin_core::browse::{ArtistReleasePage, BrowseRequest};
use rufin_core::route::CollectionCategory;

const ARTIST_RELEASE_TITLES: [&str; 6] = [
    msgid("Albums"),
    msgid("EPs"),
    msgid("Singles"),
    msgid("Collections"),
    msgid("Other releases"),
    msgid("Appears On"),
];

pub fn artist_detail_route(artist: String, album_artist: bool) -> Route {
    if album_artist {
        Route::AlbumArtistDetail(artist)
    } else {
        Route::ArtistDetail(artist)
    }
}

fn artist_discography_route(artist: String, album_artist: bool) -> Route {
    if album_artist {
        Route::AlbumArtistDiscography(artist)
    } else {
        Route::ArtistDiscography(artist)
    }
}

fn artist_tracks_route(artist: String, album_artist: bool) -> Route {
    if album_artist {
        Route::AlbumArtistTracks(artist)
    } else {
        Route::ArtistTracks(artist)
    }
}

impl CatalogUi {
    pub fn artist_detail_view(
        self: &Rc<Self>,
        artist: ArtistKey,
        album_artist: bool,
        detail: Option<ArtistOverviewData>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                "Artist",
                msgid("No music from this artist yet"),
            ));
        };
        let summary = detail.summary.clone();
        let header = artist_detail_header_restored(self, &summary, artist, album_artist);
        let favorite_count = detail.favorite_tracks.count;
        let favorite = self.searchable_track_collection(
            detail.favorite_tracks,
            LibraryListKey::ArtistTracks,
            SearchableTrackOptions {
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
        gtk_widgets::layout::configure_fill_width_clip(
            &favorite_scroller,
            gtk::PolicyType::Automatic,
        );
        favorite_scroller.set_overlay_scrolling(true);
        favorite_scroller.set_width_request(1);
        let height = 30 + favorite_count.min(4) as i32 * 64;
        favorite_scroller.set_min_content_height(height);
        favorite_scroller.set_max_content_height(height);
        favorite_scroller.set_propagate_natural_height(false);
        favorite_section.append(&favorite.mount_in_scroller(&favorite_scroller));
        let releases = Rc::new(ArtistReleaseProjections::new(
            self,
            ArtistReleaseRoutePreamble {
                header: header.widget(),
                favorite: Some((favorite_section.clone().upcast(), favorite.search())),
                favorite_present,
                empty: crate::route_layout::route_empty_view(msgid(
                    "No music from this artist yet",
                )),
            },
            ARTIST_RELEASE_TITLES,
            detail.releases,
        ));
        connect_artist_release_requests(self, &releases);
        let refresh_lane = releases.lane();
        let refresh = {
            let shell = Rc::downgrade(self);
            let favorite = favorite.clone();
            let favorite_scroller = favorite_scroller.clone();
            let releases = Rc::clone(&releases);
            let header = header.clone();
            let apply = {
                let shell = shell.clone();
                let favorite = favorite.clone();
                let releases = Rc::clone(&releases);
                let lane = Rc::clone(&refresh_lane);
                Rc::new(
                    move |generation,
                          detail: ArtistOverviewData,
                          album_settings: LibraryListSettings| {
                        let Some(shell) = shell.upgrade() else { return };
                        if !lane.finish(generation) {
                            return;
                        }
                        header.replace(&shell, detail.summary.clone());
                        favorite.replace_page(detail.favorite_tracks);
                        let present = !favorite.source_is_empty();
                        releases.set_favorite_present(present);
                        let height = 30 + favorite.source_count().min(4) as i32 * 64;
                        favorite_scroller.set_max_content_height(-1);
                        favorite_scroller.set_min_content_height(height);
                        favorite_scroller.set_max_content_height(height);
                        releases.replace_page(detail.releases);
                        releases.apply_library_list_settings(&album_settings);
                    },
                )
            };
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let album_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                let (generation, cancellation) = refresh_lane.begin();
                let database = Arc::clone(&shell.library);
                let query = releases.query(&album_settings);
                let favorite_query = favorite.query();
                let task = shell.runtime.spawn(async move {
                    let Some(summary) = database
                        .artist_rows(
                            query.source,
                            &[query.artist],
                            query.album_artist,
                            query.folder,
                            &cancellation,
                        )
                        .await?
                        .pop()
                    else {
                        return Ok::<_, library::LibraryError>(None);
                    };
                    let favorite_tracks = database
                        .song_route_page(
                            &favorite_query,
                            library::RouteSeedWindow::top(),
                            &cancellation,
                        )
                        .await?;
                    let releases = query
                        .page(&database, library::RouteSeedWindow::top(), &cancellation)
                        .await?;
                    Ok(Some(ArtistOverviewData {
                        summary,
                        favorite_tracks,
                        releases,
                    }))
                });
                let apply = Rc::downgrade(&apply);
                gtk::glib::spawn_future_local(async move {
                    let Ok(Ok(Some(detail))) = task.await else {
                        return;
                    };
                    let Some(apply) = apply.upgrade() else {
                        return;
                    };
                    apply(generation, detail, album_settings);
                });
            }) as Rc<dyn Fn()>
        };

        let resume = {
            let shell = Rc::downgrade(self);
            let favorite = favorite.clone();
            let releases = Rc::clone(&releases);
            let refresh = Rc::clone(&refresh);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let track_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                let previous = favorite.query();
                favorite.apply_library_list_settings(LibraryListKey::ArtistTracks, &track_settings);
                favorite_toolbar.apply(LibraryListKey::ArtistTracks, &track_settings);
                let album_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                let release_query_changed = releases.apply_library_list_settings(&album_settings);
                if previous != favorite.query() {
                    refresh();
                } else if release_query_changed {
                    request_artist_releases(
                        Rc::downgrade(&shell),
                        Rc::clone(&releases),
                        album_settings,
                    );
                }
            })
        };
        let download_rows = Rc::clone(&releases.sparse);

        let download_artist = Rc::clone(&header.current);
        let downloads = crate::collection_download_change(move |identity, downloaded| {
            let prefix = if album_artist {
                "album-artist:"
            } else {
                "artist:"
            };
            if identity.strip_prefix(prefix) == Some(download_artist.borrow().media_uri.as_str()) {
                let mut row = download_artist.borrow_mut();
                row.downloaded_count = if downloaded { row.track_count } else { 0 };
            }
            if let Some(uri) = identity.strip_prefix("album:") {
                download_rows.update_matching(
                    |row| {
                        row.media_uri == uri
                            && (row.downloaded_count == row.track_count) != downloaded
                    },
                    |row| row.downloaded_count = if downloaded { row.track_count } else { 0 },
                );
            }
        });
        MountedRoute::new(releases.widget(), resume)
            .with_item_scroll({
                let releases = Rc::downgrade(&releases);
                Rc::new(move || releases.upgrade().map(|releases| releases.item_scroll()))
            })
            .with_download_change(downloads)
            .with_download_change(favorite.download_change())
            .with_search_provider({
                let releases = Rc::downgrade(&releases);
                Rc::new(move || {
                    releases
                        .upgrade()
                        .and_then(|releases| releases.primary_search())
                })
            })
            .with_layout_cycle(releases.layout_cycle())
            .with_initial_demand({
                let releases = Rc::clone(&releases);
                Rc::new(move || releases.resume_initial_demand())
            })
            .with_catalog_refresh(refresh)
    }

    pub fn artist_tracks_view(
        self: &Rc<Self>,
        artist: ArtistKey,
        album_artist: bool,
        detail: Option<ArtistTracksData>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                msgid("Tracks"),
                msgid("This isn't available"),
            ));
        };
        self.artist_track_surface_restored(
            artist,
            detail.summary,
            detail.tracks,
            album_artist,
            false,
        )
    }

    pub fn artist_favorite_tracks_view(
        self: &Rc<Self>,
        artist: ArtistKey,
        album_artist: bool,
        detail: Option<ArtistTracksData>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                msgid("Favorite tracks"),
                msgid("No favorites yet"),
            ));
        };
        self.artist_track_surface_restored(
            artist,
            detail.summary,
            detail.tracks,
            album_artist,
            true,
        )
    }

    pub fn artist_discography_view(
        self: &Rc<Self>,
        detail: Option<ArtistDiscographyData>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                msgid("Discography"),
                msgid("No albums from this artist yet"),
            ));
        };
        let releases = Rc::new(ArtistReleaseProjections::new(
            self,
            ArtistReleaseRoutePreamble {
                header: artist_subroute_header(self, &detail.summary, msgid("Discography")),
                favorite: None,
                favorite_present: false,
                empty: crate::route_layout::route_empty_view(msgid(
                    "No albums from this artist yet",
                )),
            },
            ARTIST_RELEASE_TITLES,
            detail.releases,
        ));
        connect_artist_release_requests(self, &releases);
        let resume_releases = Rc::clone(&releases);
        let resume_shell = Rc::downgrade(self);
        let resume = Rc::new(move || {
            let Some(resume_shell) = resume_shell.upgrade() else {
                return;
            };
            let settings = resume_shell
                .settings
                .current
                .borrow()
                .library_list(LibraryListKey::ArtistAlbums);
            if resume_releases.apply_library_list_settings(&settings) {
                request_artist_releases(
                    Rc::downgrade(&resume_shell),
                    Rc::clone(&resume_releases),
                    settings,
                );
            }
        });
        let refresh = {
            let releases = Rc::clone(&releases);
            let shell = Rc::downgrade(self);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                request_artist_releases(Rc::downgrade(&shell), Rc::clone(&releases), settings);
            }) as Rc<dyn Fn()>
        };

        let download_rows = Rc::clone(&releases.sparse);
        let downloads = crate::collection_download_change(move |identity, downloaded| {
            let Some(uri) = identity.strip_prefix("album:") else {
                return;
            };
            download_rows.update_matching(
                |row| {
                    row.media_uri == uri && (row.downloaded_count == row.track_count) != downloaded
                },
                |row| row.downloaded_count = if downloaded { row.track_count } else { 0 },
            );
        });
        MountedRoute::new(releases.widget(), resume)
            .with_item_scroll({
                let releases = Rc::downgrade(&releases);
                Rc::new(move || releases.upgrade().map(|releases| releases.item_scroll()))
            })
            .with_download_change(downloads)
            .with_search_provider({
                let releases = Rc::downgrade(&releases);
                Rc::new(move || {
                    releases
                        .upgrade()
                        .and_then(|releases| releases.primary_search())
                })
            })
            .with_layout_cycle(releases.layout_cycle())
            .with_initial_demand({
                let releases = Rc::clone(&releases);
                Rc::new(move || releases.resume_initial_demand())
            })
            .with_catalog_refresh(refresh)
    }

    fn artist_track_surface_restored(
        self: &Rc<Self>,
        artist: ArtistKey,
        summary: ArtistRow,
        page: library::TrackRoutePage,
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
        let model = crate::track_model::TrackCollectionModel::new(
            Arc::clone(&self.library),
            self.runtime.clone(),
            page,
        );
        let (tracks_widget, tracks, toolbar) = self.scrolling_track_projection(
            model,
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
        let root = crate::route_layout::detail_route_wrapper(18);
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
        let layout_cycle = toolbar.layout_cycle();
        let refresh = tracks.connect_read(self, "mounted artist tracks");
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = Rc::clone(&tracks);
            let refresh = Rc::clone(&refresh);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                let previous = projection.query();
                projection.apply_library_list_settings(key, &settings);
                toolbar.apply(key, &settings);
                if previous != projection.query() {
                    refresh();
                }
            })
        };
        MountedRoute::new(root.upcast(), resume)
            .with_catalog_refresh(refresh)
            .with_download_change(tracks.download_change())
            .with_search(tracks.search())
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(tracks.item_navigation())
            .with_initial_demand({
                let tracks = Rc::clone(&tracks);
                Rc::new(move || tracks.resume_initial_demand())
            })
    }
}

fn connect_artist_release_requests(shell: &Rc<CatalogUi>, releases: &Rc<ArtistReleaseProjections>) {
    for index in 0..ARTIST_RELEASE_TITLES.len() {
        let Some(search) = releases.section_search(index) else {
            continue;
        };
        let weak_shell = Rc::downgrade(shell);
        let releases = Rc::downgrade(releases);
        search.connect_search_changed(move |_| {
            let (Some(shell), Some(releases)) = (weak_shell.upgrade(), releases.upgrade()) else {
                return;
            };
            let settings = shell
                .settings
                .current
                .borrow()
                .library_list(LibraryListKey::ArtistAlbums);
            request_artist_releases(Rc::downgrade(&shell), releases, settings);
        });
    }
}

fn request_artist_releases(
    shell: std::rc::Weak<CatalogUi>,
    releases: Rc<ArtistReleaseProjections>,
    settings: LibraryListSettings,
) {
    let Some(owner) = shell.upgrade() else { return };
    releases.apply_library_list_settings(&settings);
    let query = releases.query(&settings);
    let lane = releases.lane();
    let (generation, cancellation) = lane.begin();
    let database = Arc::clone(&owner.library);
    let task = owner.runtime.spawn(async move {
        query
            .page(&database, library::RouteSeedWindow::top(), &cancellation)
            .await
    });
    let lane = Rc::downgrade(&lane);
    let releases = Rc::downgrade(&releases);
    gtk::glib::spawn_future_local(async move {
        let Ok(Ok(page)) = task.await else { return };
        let Some(shell) = shell.upgrade() else { return };
        let (Some(lane), Some(releases)) = (lane.upgrade(), releases.upgrade()) else {
            return;
        };
        if !lane.finish(generation) {
            return;
        }
        releases.replace_page(page);
        shell.reapply_current_track();
    });
}

pub async fn load_artist_overview(
    database: &Database,
    route: &Route,
    track_settings: &LibraryListSettings,
    album_settings: &LibraryListSettings,
    window: library::RouteSeedWindow,
    cancellation: &ReadCancellation,
) -> Result<Option<ArtistOverviewData>, String> {
    let request = BrowseRequest::new(
        route,
        CollectionCategory::Albums,
        None,
        None,
        album_settings,
        8,
    );
    let Some(releases) = request
        .artist(database, cancellation)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let Some(summary) = database
        .artist_rows(
            releases.source,
            &[releases.artist],
            releases.album_artist,
            releases.folder,
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .pop()
    else {
        return Ok(None);
    };
    let favorites = BrowseRequest {
        favorites_only: true,
        ..BrowseRequest::new(
            route,
            CollectionCategory::Tracks,
            Some(releases.source),
            releases.folder,
            track_settings,
            8,
        )
    }
    .artist_songs(&releases);
    let favorite_tracks = database
        .song_route_page(&favorites, library::RouteSeedWindow::top(), cancellation)
        .await
        .map_err(|error| error.to_string())?;
    let releases = releases
        .page(database, window, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(ArtistOverviewData {
        summary,
        favorite_tracks,
        releases,
    }))
}

pub async fn load_artist_discography(
    database: &Database,
    route: &Route,
    settings: &LibraryListSettings,
    window: library::RouteSeedWindow,
    cancellation: &ReadCancellation,
) -> Result<Option<ArtistDiscographyData>, String> {
    let request = BrowseRequest::new(route, CollectionCategory::Albums, None, None, settings, 8);
    let Some(releases) = request
        .artist(database, cancellation)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let Some(summary) = database
        .artist_rows(
            releases.source,
            &[releases.artist],
            releases.album_artist,
            releases.folder,
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .pop()
    else {
        return Ok(None);
    };
    let releases = releases
        .page(database, window, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(ArtistDiscographyData { summary, releases }))
}

pub async fn load_artist_tracks(
    database: &Database,
    route: &Route,
    settings: &LibraryListSettings,
    window: library::RouteSeedWindow,
    cancellation: &ReadCancellation,
) -> Result<Option<ArtistTracksData>, String> {
    let request = BrowseRequest::new(route, CollectionCategory::Tracks, None, None, settings, 8);
    let Some(artist) = request
        .artist(database, cancellation)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let Some(summary) = database
        .artist_rows(
            artist.source,
            &[artist.artist],
            artist.album_artist,
            artist.folder,
            cancellation,
        )
        .await
        .map_err(|error| error.to_string())?
        .pop()
    else {
        return Ok(None);
    };
    let tracks = database
        .song_route_page(&request.artist_songs(&artist), window, cancellation)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(ArtistTracksData { summary, tracks }))
}

#[derive(Clone)]
struct ArtistDetailHeaderProjection {
    album_artist: bool,
    root: gtk::Widget,
    current: Rc<RefCell<ArtistRow>>,
    showcase: DetailShowcaseView,
    album_count: gtk::Label,
    track_count: gtk::Label,
    cover: MediaCoverProjection,
}

impl ArtistDetailHeaderProjection {
    fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    fn replace(&self, shell: &Rc<CatalogUi>, artist: ArtistRow) {
        self.showcase.set_title(&artist.name);
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
        self.showcase
            .replace_external_links(super::detail_showcase::artist_external_links(
                shell,
                &artist,
                self.album_artist,
            ));
        shell.update_visible_favorite_buttons(
            &library::FavoriteTarget::Artist(artist.media_uri.clone()),
            artist.favorite,
        );
        self.current.replace(artist);
    }
}

fn artist_detail_header_restored(
    shell: &Rc<CatalogUi>,
    artist: &ArtistRow,
    artist_key: ArtistKey,
    album_artist: bool,
) -> ArtistDetailHeaderProjection {
    let current = Rc::new(RefCell::new(artist.clone()));
    let width = crate::route_layout::detail_route_inner_width_for_viewport(
        (shell.route_width)(),
        PRIMARY_ROUTE_MARGIN_START,
    );
    let cover = media_cover_projection(
        shell,
        artist
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        crate::route_layout::detail_showcase_cover_size(width),
        "artist-detail-cover",
    );
    let showcase = DetailShowcaseView::new(
        "artist-detail-showcase",
        stable_seed(&artist.object_id),
        "Artist",
        true,
        &artist.name,
    );
    let radio = detail_radio_button();
    let radio_controller = shell.playback.clone();
    radio.connect_clicked(move |_| {
        radio_controller.play_radio(RadioPlayRequest::now(if album_artist {
            RadioSeed::AlbumArtist(artist_key)
        } else {
            RadioSeed::Artist(artist_key)
        }));
    });
    showcase.append_kind_control(&radio);
    let counts = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    counts.add_css_class("artist-count-row");
    counts.set_halign(gtk::Align::Start);
    let (albums, album_count) = artist_count_button(
        "rufin-albums-symbolic",
        &album_count_text(artist.album_count.max(0) as u64),
    );
    let album_shell = Rc::clone(shell);
    let album_uri = artist.media_uri.clone();
    albums.connect_clicked(move |_| {
        album_shell.navigate(artist_discography_route(album_uri.clone(), album_artist))
    });
    counts.append(&albums);
    let (tracks, track_count) = artist_count_button(
        "rufin-tracks-symbolic",
        &track_count_text(artist.track_count.max(0) as u64),
    );
    let track_shell = Rc::clone(shell);
    let track_uri = artist.media_uri.clone();
    tracks.connect_clicked(move |_| {
        track_shell.navigate(artist_tracks_route(track_uri.clone(), album_artist))
    });
    counts.append(&tracks);
    showcase.append_detail(&counts);

    let actions = showcase.actions();
    actions.add_css_class("artist-detail-actions");
    actions.set_halign(gtk::Align::Start);
    let target = gtk_widgets::selection::artist_play_request(artist, album_artist);
    let play_shell = Rc::clone(shell);
    let play: CollectionPlay = Rc::new(move |placement, shuffled| {
        (play_shell.media_menus.play_target)(&target, placement, shuffled);
    });
    let controls = detail_playback_controls(
        &actions,
        msgid("Play artist"),
        Some(artist.favorite),
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
    let favorite_media_uri = artist.media_uri.clone();
    for button in [favorite, hover_favorite] {
        shell.register_favorite_button(artist_favorite_key(&favorite_media_uri), &button);
        let favorite_shell = Rc::clone(shell);
        let favorite_media_uri = favorite_media_uri.clone();
        button.connect_clicked(move |button| {
            favorite_shell.set_favorite_with_feedback(
                library::FavoriteTarget::Artist(favorite_media_uri.clone()),
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
            &menu_shell.media_menus,
            artist,
            album_artist,
            Some(Rc::clone(&menu_play)),
            position,
        );
    });
    showcase.replace_external_links(super::detail_showcase::artist_external_links(
        shell,
        artist,
        album_artist,
    ));
    let root = detail_showcase_frame(media_showcase(MediaShowcase {
        view: showcase.clone(),
        initial_width: width,
        cover: cover.clone(),
        cover_controls: controls,
        context_menu: Some(context_menu),
        actions_min_cover_size: None,
    }));
    ArtistDetailHeaderProjection {
        album_artist,
        root,
        current,
        showcase,
        album_count,
        track_count,
        cover,
    }
}

fn artist_subroute_header(
    shell: &Rc<CatalogUi>,
    artist: &ArtistRow,
    kind_text: &str,
) -> gtk::Widget {
    let width = crate::route_layout::detail_route_inner_width_for_viewport(
        (shell.route_width)(),
        PRIMARY_ROUTE_MARGIN_START,
    );
    let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
    header.add_css_class("detail-showcase");
    header.add_css_class("artist-detail-showcase");
    if width < 520 {
        header.add_css_class("detail-showcase-tiny");
    }
    gtk_widgets::artwork::presentation::add_album_seed_gradient_class(
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
    let frame = super::detail_showcase::detail_showcase_frame(header.upcast());
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
        let key = library::source_entity_uri(&library::SourceId::new("source"), "artist", "7");
        assert_eq!(
            artist_detail_route(key.clone(), false),
            Route::ArtistDetail(key.clone())
        );
        assert_eq!(
            artist_detail_route(key.clone(), true),
            Route::AlbumArtistDetail(key)
        );
        let key = library::ArtistKey::from_raw(7);
        for album_artist in [false, true] {
            let request =
                gtk_widgets::selection::collection_play_request(library::SongScope::Artist {
                    key,
                    album_artist,
                });
            assert!(
                matches!(request.selection(), Some(library::SongSelection::Query { query: library::SongQuery { scope: library::SongScope::Artist { key: value, album_artist: role }, .. }, .. }) if value == key && role == album_artist)
            );
        }
    }
}

use gtk_media_menus::media_menus::CollectionPlay;
