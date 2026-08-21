use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use ::library::{
    AlbumSummary, Artist, ArtistDiscography, ArtistId, ArtistOverview, ArtistSummary, ArtistTracks,
    FavoriteItemId, Library, MusicFolderId, RadioSeed,
};
use adw::prelude::*;
use artwork::ArtworkBinding;

use crate::LibraryListKey;
use crate::LibraryListSettings;
use crate::favorites::{
    artist_favorite_key, favorite_button_is_active, favorite_icon_button,
    set_favorite_button_active,
};
use crate::layout::{configure_fill_width_clip, width_allocation_owner};
use crate::localization::{bind_label_text_with, localized_label};
use crate::shell::Shell;
use crate::shell::actions::{ActionButtonVariant, configure_action_button};
use crate::shell::cover::presentation::{add_album_seed_gradient_class, stable_seed};
use crate::shell::route::{LatestMountedRouteRead, MountedRoute, SelectedRouteIdentity};
use localization::{album_count_text, msgid, track_count_text};
use playback::RadioPlayRequest;

use super::artist_releases::{
    ArtistReleaseProjections, ArtistReleaseRoutePreamble, ArtistRouteSearchTarget,
};
use super::collection_context::present_artist_context_menu;
use super::collections::{
    COMPACT_TRACK_TABLE_HEADER_HEIGHT, CollectionPlay, PlaybackTarget,
    configure_compact_track_table_scroller, library_route_inset,
};
use super::detail_showcase::{
    DetailCoverProjection, DetailExternalLinksProjection, MediaDetailShowcase,
    artist_external_links, detail_action_row, detail_cover_projection, detail_playback_controls,
    detail_radio_button, detail_showcase_frame_with_back, fit_detail_text,
    fitted_detail_title_label, mark_tiny_detail_showcase, media_detail_showcase,
};
use super::models::sort_albums;
use super::route::Route;
use super::route_layout::{
    PRIMARY_ROUTE_HORIZONTAL_INSET, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
    detail_route_inner_width, detail_showcase_cover_size,
};
use super::routes::{SearchableTrackOptions, install_embedded_track_scroll_latch};
use super::track_model::{
    PreparedTrackProjection, TrackProjectionRequest, prepare_track_projection,
};

const ARTIST_COUNT_ICON_SIZE: i32 = 16;

#[derive(Clone)]
struct ArtistOverviewReadRequest {
    identity: SelectedRouteIdentity,
    favorite_tracks: TrackProjectionRequest,
    albums: LibraryListSettings,
}

struct PreparedArtistOverview {
    summary: ArtistSummary,
    favorite_tracks: PreparedTrackProjection,
    albums: Arc<[AlbumSummary]>,
    appears_on: Arc<[AlbumSummary]>,
}

#[derive(Clone)]
struct ArtistDiscographyReadRequest {
    identity: SelectedRouteIdentity,
    albums: LibraryListSettings,
}

#[derive(Clone)]
struct ArtistTracksReadRequest {
    identity: SelectedRouteIdentity,
    tracks: TrackProjectionRequest,
}

struct PreparedArtistTracks {
    summary: ArtistSummary,
    tracks: PreparedTrackProjection,
}

#[derive(Clone, Copy)]
struct ArtistSummaryCounts {
    album_count: u32,
    track_count: u32,
}

#[derive(Clone)]
struct ArtistSummaryFacts(Rc<Cell<ArtistSummaryCounts>>);

impl ArtistSummaryFacts {
    fn new(album_count: u32, track_count: u32) -> Self {
        Self(Rc::new(Cell::new(ArtistSummaryCounts {
            album_count,
            track_count,
        })))
    }

    fn replace(&self, album_count: u32, track_count: u32) {
        self.0.set(ArtistSummaryCounts {
            album_count,
            track_count,
        });
    }

    fn album_text(&self) -> String {
        album_count_text(u64::from(self.0.get().album_count))
    }

    fn track_text(&self) -> String {
        track_count_text(u64::from(self.0.get().track_count))
    }

    fn summary_text(&self) -> String {
        let facts = self.0.get();
        artist_summary_text(facts.album_count, facts.track_count)
    }
}

#[derive(Clone)]
struct ArtistDetailHeaderProjection {
    root: gtk::Widget,
    external_links: DetailExternalLinksProjection,
    summary: Rc<RefCell<ArtistSummary>>,
    title: gtk::Label,
    album_count: gtk::Label,
    track_count: gtk::Label,
    cover: DetailCoverProjection,
    facts: ArtistSummaryFacts,
}

impl ArtistDetailHeaderProjection {
    fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    fn apply_external_link_settings(&self, shell: &Rc<Shell>) {
        self.external_links
            .replace(artist_external_links(shell, &self.summary.borrow().artist));
    }

    fn replace(&self, shell: &Rc<Shell>, summary: ArtistSummary) {
        let artist = Arc::clone(&summary.artist);
        self.facts.replace(summary.album_count, summary.track_count);
        self.title.set_text(&artist.name);
        fit_detail_text(&self.title, &artist.name);
        self.album_count.set_text(&self.facts.album_text());
        self.track_count.set_text(&self.facts.track_text());
        self.cover
            .replace(shell, ArtworkBinding::artist(&summary.artwork));
        shell.update_visible_favorite_buttons(
            &FavoriteItemId::Artist(artist.id.clone()),
            artist.favorite,
        );
        self.summary.replace(summary);
        self.apply_external_link_settings(shell);
    }
}

#[derive(Clone)]
struct ArtistSubrouteHeaderProjection {
    root: gtk::Widget,
    title: gtk::Label,
    summary: gtk::Label,
}

impl ArtistSubrouteHeaderProjection {
    fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    fn replace(&self, artist: &Artist, summary: &str) {
        self.title.set_text(&artist.name);
        fit_detail_text(&self.title, &artist.name);
        self.summary.set_text(summary);
    }
}

impl Shell {
    pub(crate) fn artist_detail_view(
        self: &Rc<Self>,
        artist_id: ArtistId,
        detail: Option<ArtistOverview>,
        loaded: Arc<Library>,
        music_folder_id: Option<MusicFolderId>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view("Artist", msgid("This isn't available")),
            );
        };
        let ArtistOverview {
            summary,
            favorite_tracks,
            albums,
            appears_on,
        } = detail;
        let applied_external_link_settings = Rc::new(RefCell::new(
            self.settings.current.borrow().external_site_links.clone(),
        ));
        let wrapper = super::route_layout::detail_route_wrapper(0);
        let header = self.artist_detail_header(summary);

        let favorite_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        favorite_section.append(&section_heading(msgid("Favorite tracks")));
        let favorite_scroller = gtk::ScrolledWindow::new();
        let resize_favorite_scroller = favorite_scroller.clone();
        let resize_favorite_tracks: Rc<dyn Fn(usize)> = Rc::new(move |row_count| {
            configure_compact_track_table_scroller(&resize_favorite_scroller, row_count);
        });
        let favorite_projection = self.searchable_track_collection(
            favorite_tracks,
            LibraryListKey::ArtistTracks,
            SearchableTrackOptions {
                on_visible_count_changed: Some(resize_favorite_tracks),
                context_id: format!("artist:{}|favorites", artist_id.as_str()),
                content_inset: PRIMARY_ROUTE_HORIZONTAL_INSET,
                fixed_layout: Some(crate::LibraryLayout::Row),
            },
        );
        let favorite_panel = gtk::Box::new(gtk::Orientation::Vertical, 10);
        favorite_panel.set_widget_name("artist-favorites");
        favorite_panel.set_hexpand(true);
        favorite_panel.set_halign(gtk::Align::Fill);
        let favorite_toolbar = self
            .library_toolbar_projection(LibraryListKey::ArtistTracks, favorite_projection.search());
        favorite_toolbar.set_layout_control_visible(false);
        favorite_panel.append(&favorite_toolbar.widget());
        configure_fill_width_clip(&favorite_scroller, gtk::PolicyType::Automatic);
        favorite_scroller.set_overlay_scrolling(true);
        install_embedded_track_scroll_latch(&favorite_scroller, COMPACT_TRACK_TABLE_HEADER_HEIGHT);
        favorite_scroller.set_width_request(1);
        favorite_scroller.set_hexpand(true);
        favorite_scroller.set_halign(gtk::Align::Fill);
        favorite_panel.append(&favorite_projection.mount_in_scroller(&favorite_scroller));
        favorite_section.append(&favorite_panel);

        let releases = ArtistReleaseProjections::new(
            self,
            ArtistReleaseRoutePreamble {
                header: header.widget(),
                favorite: Some((favorite_section.upcast(), favorite_projection.search())),
                favorite_present: !favorite_projection.source_is_empty(),
                empty: self.placeholder_view("Artist", msgid("No music from this artist yet")),
            },
            albums,
            appears_on,
            format!("artist:{}|releases", artist_id.as_str()),
        );
        set_artist_route_search(self, releases.primary_search());
        wrapper.append(&releases.widget());
        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("content"));
        route_stack.add_named(
            &self.placeholder_view("Artist", msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("content");

        let identity = self.mounted_route_read_identity(
            Route::ArtistDetail(artist_id.clone()),
            &loaded,
            music_folder_id.clone(),
        );
        let apply = {
            let shell = Rc::clone(self);
            let route_stack = route_stack.clone();
            let header = header.clone();
            let favorite_projection = favorite_projection.clone();
            let releases = releases.clone();
            Rc::new(
                move |request: ArtistOverviewReadRequest,
                      result: Result<Option<PreparedArtistOverview>, String>| {
                    if !shell.mounted_route_read_is_current(&request.identity) {
                        return;
                    }
                    let next = match result {
                        Ok(next) => next,
                        Err(error) => {
                            tracing::warn!(%error, "failed to refresh the mounted Artist route");
                            return;
                        }
                    };
                    let Some(next) = next else {
                        shell.set_route_search(None);
                        route_stack.set_visible_child_name("missing");
                        return;
                    };
                    if !favorite_projection.replace_prepared(next.favorite_tracks) {
                        return;
                    }
                    header.replace(&shell, next.summary);
                    releases.replace_prepared(
                        next.albums,
                        next.appears_on,
                        !favorite_projection.source_is_empty(),
                    );
                    set_artist_route_search(&shell, releases.primary_search());
                    route_stack.set_visible_child_name("content");
                },
            )
        };
        let load = {
            let loaded = Arc::clone(&loaded);
            let artist_id = artist_id.clone();
            let music_folder_id = music_folder_id.clone();
            Arc::new(move |request: &ArtistOverviewReadRequest| {
                load_artist_overview(
                    &loaded,
                    &artist_id,
                    music_folder_id.as_ref(),
                    &request.favorite_tracks.settings,
                    &request.albums,
                )
                .and_then(|detail| {
                    detail
                        .map(
                            |ArtistOverview {
                                 summary,
                                 favorite_tracks,
                                 albums,
                                 appears_on,
                             }| {
                                let favorite_tracks = prepare_track_projection(
                                    favorite_tracks,
                                    request.favorite_tracks.clone(),
                                )
                                .map_err(|error| error.to_string())?;
                                Ok(PreparedArtistOverview {
                                    summary,
                                    favorite_tracks,
                                    albums,
                                    appears_on,
                                })
                            },
                        )
                        .transpose()
                })
            })
        };
        let read = LatestMountedRouteRead::new_with_request(apply, load, "mounted Artist route");
        {
            let read = Rc::downgrade(&read);
            let identity = identity.clone();
            let shell = Rc::clone(self);
            favorite_projection.connect_search_request(move |favorite_tracks| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with_if_running(ArtistOverviewReadRequest {
                    identity: identity.clone(),
                    favorite_tracks,
                    albums: shell
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::ArtistAlbums),
                });
            });
        }
        let resume = {
            let shell = Rc::clone(self);
            let applied_external_link_settings = Rc::clone(&applied_external_link_settings);
            let header = header.clone();
            let favorite_projection = favorite_projection.clone();
            let favorite_toolbar = favorite_toolbar.clone();
            let releases = releases.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move || {
                let external_link_settings =
                    shell.settings.current.borrow().external_site_links.clone();
                if *applied_external_link_settings.borrow() != external_link_settings {
                    header.apply_external_link_settings(&shell);
                    applied_external_link_settings.replace(external_link_settings);
                }
                let track_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                favorite_projection
                    .apply_library_list_settings(LibraryListKey::ArtistTracks, &track_settings);
                favorite_toolbar.apply(LibraryListKey::ArtistTracks, &track_settings);
                let album_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                releases.apply_library_list_settings(LibraryListKey::ArtistAlbums, &album_settings);
                read.request_with_if_running(ArtistOverviewReadRequest {
                    identity: identity.clone(),
                    favorite_tracks: favorite_projection.projection_request(),
                    albums: album_settings,
                });
            })
        };
        let update = {
            let artist_id = artist_id.clone();
            let favorite_projection = favorite_projection.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            let shell = Rc::clone(self);
            let music_folder_id = music_folder_id.clone();
            Rc::new(move |update: &crate::runtime::SelectedLibraryUpdate| {
                if !update.change.artists.contains(&artist_id)
                    && !update.change.artist_releases.contains(&artist_id)
                {
                    let replacements = update.change.tracks.as_slice();
                    if replacements.is_empty()
                        || favorite_projection.apply_track_replacement(replacements, |track| {
                            track.favorite
                                && track
                                    .relations
                                    .artists
                                    .iter()
                                    .chain(track.relations.album_artists.iter())
                                    .any(|artist| artist.id == artist_id)
                                && music_folder_id.as_ref().is_none_or(|folder_id| {
                                    track.relations.music_folders.contains(folder_id)
                                })
                        })
                    {
                        return;
                    }
                }
                read.request_with(ArtistOverviewReadRequest {
                    identity: identity.clone(),
                    favorite_tracks: favorite_projection.projection_request(),
                    albums: shell
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::ArtistAlbums),
                });
            })
        };
        MountedRoute::new(route_stack.upcast(), resume).with_library_update(update)
    }

    pub(crate) fn artist_discography_view(
        self: &Rc<Self>,
        artist_id: ArtistId,
        detail: Option<ArtistDiscography>,
        loaded: Arc<Library>,
        music_folder_id: Option<MusicFolderId>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view(msgid("Discography"), msgid("This isn't available")),
            );
        };
        let facts = ArtistSummaryFacts::new(detail.summary.album_count, detail.summary.track_count);
        let wrapper = super::route_layout::detail_route_wrapper(0);
        let header = self.artist_subroute_header(
            &detail.summary.artist,
            msgid("Discography"),
            &facts.summary_text(),
        );
        let localized_facts = facts.clone();
        bind_label_text_with(&header.summary, move || localized_facts.summary_text());
        let releases = ArtistReleaseProjections::new(
            self,
            ArtistReleaseRoutePreamble {
                header: header.widget(),
                favorite: None,
                favorite_present: false,
                empty: self.placeholder_view(
                    msgid("Discography"),
                    msgid("No albums from this artist yet"),
                ),
            },
            detail.albums,
            detail.appears_on,
            format!("artist:{}|releases", artist_id.as_str()),
        );
        set_artist_route_search(self, releases.primary_search());
        wrapper.append(&releases.widget());
        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("content"));
        route_stack.add_named(
            &self.placeholder_view(msgid("Discography"), msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("content");
        let identity = self.mounted_route_read_identity(
            Route::ArtistDiscography(artist_id.clone()),
            &loaded,
            music_folder_id.clone(),
        );
        let apply = {
            let shell = Rc::clone(self);
            let route_stack = route_stack.clone();
            let header = header.clone();
            let releases = releases.clone();
            let facts = facts.clone();
            Rc::new(
                move |request: ArtistDiscographyReadRequest,
                      result: Result<Option<ArtistDiscography>, String>| {
                    if !shell.mounted_route_read_is_current(&request.identity) {
                        return;
                    }
                    let next = match result {
                        Ok(next) => next,
                        Err(error) => {
                            tracing::warn!(%error, "failed to refresh the mounted Discography route");
                            return;
                        }
                    };
                    let Some(next) = next else {
                        shell.set_route_search(None);
                        route_stack.set_visible_child_name("missing");
                        return;
                    };
                    facts.replace(next.summary.album_count, next.summary.track_count);
                    header.replace(&next.summary.artist, &facts.summary_text());
                    releases.replace_prepared(next.albums, next.appears_on, false);
                    set_artist_route_search(&shell, releases.primary_search());
                    route_stack.set_visible_child_name("content");
                },
            )
        };
        let load = {
            let loaded = Arc::clone(&loaded);
            let artist_id = artist_id.clone();
            let music_folder_id = music_folder_id.clone();
            Arc::new(move |request: &ArtistDiscographyReadRequest| {
                load_artist_discography(
                    &loaded,
                    &artist_id,
                    music_folder_id.as_ref(),
                    &request.albums,
                )
            })
        };
        let read =
            LatestMountedRouteRead::new_with_request(apply, load, "mounted Discography route");
        let resume = {
            let shell = Rc::clone(self);
            let releases = releases.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move || {
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                releases.apply_library_list_settings(LibraryListKey::ArtistAlbums, &settings);
                read.request_with_if_running(ArtistDiscographyReadRequest {
                    identity: identity.clone(),
                    albums: settings,
                });
            })
        };
        let update = {
            let shell = Rc::clone(self);
            let artist_id = artist_id.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move |update: &crate::runtime::SelectedLibraryUpdate| {
                if !update.change.artists.contains(&artist_id)
                    && !update.change.artist_releases.contains(&artist_id)
                {
                    return;
                }
                read.request_with(ArtistDiscographyReadRequest {
                    identity: identity.clone(),
                    albums: shell
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::ArtistAlbums),
                });
            })
        };
        MountedRoute::new(route_stack.upcast(), resume).with_library_update(update)
    }

    pub(crate) fn artist_tracks_view(
        self: &Rc<Self>,
        artist_id: ArtistId,
        detail: Option<ArtistTracks>,
        loaded: Arc<Library>,
        music_folder_id: Option<MusicFolderId>,
    ) -> MountedRoute {
        let Some(detail) = detail else {
            return MountedRoute::static_widget(
                self.placeholder_view("Tracks", msgid("This isn't available")),
            );
        };
        let facts = ArtistSummaryFacts::new(detail.summary.album_count, detail.summary.track_count);
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 14);
        wrapper.add_css_class("route-content");
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);
        wrapper.set_hexpand(true);
        wrapper.set_vexpand(true);
        wrapper.set_width_request(1);

        let header =
            self.artist_subroute_header(&detail.summary.artist, "Tracks", &facts.summary_text());
        let localized_facts = facts.clone();
        bind_label_text_with(&header.summary, move || localized_facts.summary_text());
        wrapper.append(&library_route_inset(header.widget()));

        let track_projection = self.searchable_track_collection(
            detail.tracks,
            LibraryListKey::ArtistTracks,
            SearchableTrackOptions {
                on_visible_count_changed: None,
                context_id: format!("artist:{}", artist_id.as_str()),
                content_inset: PRIMARY_ROUTE_HORIZONTAL_INSET,
                fixed_layout: None,
            },
        );
        let track_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        track_section.set_widget_name("artist-tracks");
        track_section.set_hexpand(true);
        track_section.set_halign(gtk::Align::Fill);
        track_section.set_vexpand(true);
        let track_toolbar = self
            .library_toolbar_projection(LibraryListKey::ArtistTracks, track_projection.search());
        track_section.append(&library_route_inset(track_toolbar.widget()));
        self.set_route_search(Some(track_projection.search()));
        let item_navigation = track_projection.item_navigation();
        track_section.append(&track_projection.scrolling_widget());
        let track_stack = gtk::Stack::new();
        track_stack.set_hexpand(true);
        track_stack.set_vexpand(true);
        track_stack.add_named(&track_section, Some("tracks"));
        track_stack.add_named(
            &library_route_inset(self.placeholder_view("Tracks", msgid("No tracks here yet"))),
            Some("empty"),
        );
        track_stack.set_visible_child_name(if track_projection.source_is_empty() {
            "empty"
        } else {
            "tracks"
        });
        wrapper.append(&track_stack);
        let route_stack = gtk::Stack::new();
        route_stack.set_hexpand(true);
        route_stack.set_vexpand(true);
        route_stack.add_named(&wrapper, Some("content"));
        route_stack.add_named(
            &self.placeholder_view("Tracks", msgid("This isn't available")),
            Some("missing"),
        );
        route_stack.set_visible_child_name("content");

        let identity = self.mounted_route_read_identity(
            Route::ArtistTracks(artist_id.clone()),
            &loaded,
            music_folder_id.clone(),
        );
        let apply = {
            let shell = Rc::clone(self);
            let route_stack = route_stack.clone();
            let header = header.clone();
            let facts = facts.clone();
            let track_projection = track_projection.clone();
            let track_stack = track_stack.clone();
            Rc::new(
                move |request: ArtistTracksReadRequest,
                      result: Result<Option<PreparedArtistTracks>, String>| {
                    if !shell.mounted_route_read_is_current(&request.identity) {
                        return;
                    }
                    let next = match result {
                        Ok(next) => next,
                        Err(error) => {
                            tracing::warn!(
                                %error,
                                "failed to refresh the mounted Artist Tracks route"
                            );
                            return;
                        }
                    };
                    let Some(next) = next else {
                        shell.set_route_search(None);
                        route_stack.set_visible_child_name("missing");
                        return;
                    };
                    if !track_projection.replace_prepared(next.tracks) {
                        return;
                    }
                    facts.replace(next.summary.album_count, next.summary.track_count);
                    header.replace(&next.summary.artist, &facts.summary_text());
                    track_stack.set_visible_child_name(if track_projection.source_is_empty() {
                        "empty"
                    } else {
                        "tracks"
                    });
                    shell.set_route_search(Some(track_projection.search()));
                    route_stack.set_visible_child_name("content");
                },
            )
        };
        let load = {
            let loaded = Arc::clone(&loaded);
            let artist_id = artist_id.clone();
            let music_folder_id = music_folder_id.clone();
            Arc::new(move |request: &ArtistTracksReadRequest| {
                load_artist_tracks(
                    &loaded,
                    &artist_id,
                    music_folder_id.as_ref(),
                    &request.tracks.settings,
                )
                .and_then(|detail| {
                    detail
                        .map(|ArtistTracks { summary, tracks }| {
                            prepare_track_projection(tracks, request.tracks.clone())
                                .map(|tracks| PreparedArtistTracks { summary, tracks })
                                .map_err(|error| error.to_string())
                        })
                        .transpose()
                })
            })
        };
        let read =
            LatestMountedRouteRead::new_with_request(apply, load, "mounted Artist Tracks route");
        {
            let read = Rc::downgrade(&read);
            let identity = identity.clone();
            track_projection.connect_search_request(move |tracks| {
                let Some(read) = read.upgrade() else {
                    return;
                };
                read.request_with_if_running(ArtistTracksReadRequest {
                    identity: identity.clone(),
                    tracks,
                });
            });
        }
        let resume = {
            let shell = Rc::clone(self);
            let track_projection = track_projection.clone();
            let track_toolbar = track_toolbar.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            Rc::new(move || {
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                track_projection
                    .apply_library_list_settings(LibraryListKey::ArtistTracks, &settings);
                track_toolbar.apply(LibraryListKey::ArtistTracks, &settings);
                read.request_with_if_running(ArtistTracksReadRequest {
                    identity: identity.clone(),
                    tracks: track_projection.projection_request(),
                });
            })
        };
        let update = {
            let artist_id = artist_id.clone();
            let track_projection = track_projection.clone();
            let read = Rc::clone(&read);
            let identity = identity.clone();
            let music_folder_id = music_folder_id.clone();
            Rc::new(move |update: &crate::runtime::SelectedLibraryUpdate| {
                let replacements = update.change.tracks.as_slice();
                if !update.change.artists.contains(&artist_id) {
                    if replacements.is_empty()
                        || track_projection.apply_track_replacement(replacements, |track| {
                            track
                                .relations
                                .artists
                                .iter()
                                .chain(track.relations.album_artists.iter())
                                .any(|artist| artist.id == artist_id)
                                && music_folder_id.as_ref().is_none_or(|folder_id| {
                                    track.relations.music_folders.contains(folder_id)
                                })
                        })
                    {
                        return;
                    }
                }
                read.request_with(ArtistTracksReadRequest {
                    identity: identity.clone(),
                    tracks: track_projection.projection_request(),
                });
            })
        };
        MountedRoute::new(route_stack.upcast(), resume)
            .with_item_navigation(item_navigation)
            .with_library_update(update)
    }

    fn artist_detail_header(
        self: &Rc<Self>,
        summary: ArtistSummary,
    ) -> ArtistDetailHeaderProjection {
        let artist = Arc::clone(&summary.artist);
        let facts = ArtistSummaryFacts::new(summary.album_count, summary.track_count);
        let current_summary = Rc::new(RefCell::new(summary));
        let content_width = detail_route_inner_width(self, PRIMARY_ROUTE_MARGIN_START);
        let cover_size = detail_showcase_cover_size(content_width);
        let seed = stable_seed(artist.id.as_str());
        let cover = detail_cover_projection(
            self,
            ArtworkBinding::artist(&current_summary.borrow().artwork),
            cover_size,
            "artist-detail-cover",
        );
        let counts = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        counts.add_css_class("artist-count-row");
        counts.set_halign(gtk::Align::Start);
        let (albums, album_count) =
            artist_count_button_with_label("rufin-albums-symbolic", &facts.album_text());
        let localized_album_count = facts.clone();
        bind_label_text_with(&album_count, move || localized_album_count.album_text());
        let shell = Rc::clone(self);
        let artist_id = artist.id.clone();
        albums.connect_clicked(move |_| {
            shell.navigate(Route::ArtistDiscography(artist_id.clone()));
        });
        counts.append(&albums);
        let (tracks_button, track_count) =
            artist_count_button_with_label("rufin-tracks-symbolic", &facts.track_text());
        let localized_track_count = facts.clone();
        bind_label_text_with(&track_count, move || localized_track_count.track_text());
        let shell = Rc::clone(self);
        let artist_id = artist.id.clone();
        tracks_button.connect_clicked(move |_| {
            shell.navigate(Route::ArtistTracks(artist_id.clone()));
        });
        counts.append(&tracks_button);

        let text_stack = gtk::Box::new(gtk::Orientation::Vertical, 8);
        text_stack.set_hexpand(true);
        text_stack.set_halign(gtk::Align::Fill);
        text_stack.set_width_request(1);
        let kind_row = self.artist_detail_kind_row(artist.id.clone());
        let title = fitted_detail_title_label(&artist.name);
        text_stack.append(&kind_row);
        text_stack.append(&title);
        text_stack.append(&counts);

        let actions = detail_action_row();
        actions.add_css_class("artist-detail-actions");
        actions.set_halign(gtk::Align::Start);
        let playback_target = PlaybackTarget::Artist(artist.id.clone());
        let controller = self.products.playback.queue.clone();
        let play_shell = Rc::clone(self);
        let play: CollectionPlay = Rc::new(move |placement, shuffled_start| {
            if let Some(request) =
                playback_target.play_request(&play_shell, placement, shuffled_start)
            {
                controller.play_loaded(request);
            }
        });
        let cover_controls = detail_playback_controls(
            &actions,
            msgid("Play artist"),
            Some(artist.favorite),
            true,
            Rc::clone(&play),
        );

        let favorite = favorite_icon_button("Favorite");
        configure_action_button(&favorite, ActionButtonVariant::DetailFavorite, None);
        set_favorite_button_active(&favorite, artist.favorite);
        actions.append(&favorite);
        let hover_favorite = cover_controls
            .favorite
            .as_ref()
            .expect("artist detail has a Favorite cover control")
            .clone();
        for button in [favorite, hover_favorite] {
            self.register_favorite_button(artist_favorite_key(&artist.id), &button);
            let shell = Rc::clone(self);
            let artist_id = artist.id.clone();
            button.connect_clicked(move |button| {
                shell.set_favorite_with_feedback(
                    FavoriteItemId::Artist(artist_id.clone()),
                    !favorite_button_is_active(button),
                    Some(button),
                );
            });
        }

        let menu_shell = Rc::clone(self);
        let menu_summary = Rc::clone(&current_summary);
        let menu_play = Rc::clone(&play);
        let context_menu: crate::interactions::ContextMenuOpen =
            Rc::new(move |target, position| {
                let summary = menu_summary.borrow().clone();
                present_artist_context_menu(
                    target,
                    &menu_shell,
                    summary,
                    Some(Rc::clone(&menu_play)),
                    position,
                );
            });

        let external_links =
            DetailExternalLinksProjection::new(None, artist_external_links(self, &artist));
        let root = media_detail_showcase(
            self,
            MediaDetailShowcase {
                route_class: "artist-detail-showcase",
                seed,
                initial_width: content_width,
                cover: cover.clone(),
                cover_controls,
                context_menu: Some(context_menu),
                external_links: external_links.clone(),
                text_stack: text_stack.upcast(),
                actions: actions.upcast(),
            },
        );
        ArtistDetailHeaderProjection {
            root,
            external_links,
            summary: current_summary,
            title,
            album_count,
            track_count,
            cover,
            facts,
        }
    }

    fn artist_detail_kind_row(self: &Rc<Self>, artist_id: ArtistId) -> gtk::Box {
        let kind = localized_label("Artist");
        kind.add_css_class("eyebrow");
        kind.set_xalign(0.0);
        kind.set_halign(gtk::Align::Start);
        kind.set_valign(gtk::Align::Center);
        kind.set_margin_end(6);

        let row = gtk::Box::new(gtk::Orientation::Horizontal, 2);
        row.add_css_class("album-detail-kind-row");
        row.add_css_class("album-detail-genre-row");
        row.set_valign(gtk::Align::Center);
        row.set_halign(gtk::Align::Start);
        row.append(&kind);
        let radio = detail_radio_button();
        let controller = self.products.playback.radio.clone();
        radio.connect_clicked(move |_| {
            controller.play_radio(RadioPlayRequest::now(RadioSeed::Artist(artist_id.clone())));
        });
        row.append(&radio);
        row
    }

    fn artist_subroute_header(
        self: &Rc<Self>,
        artist: &Artist,
        kind: &str,
        summary: &str,
    ) -> ArtistSubrouteHeaderProjection {
        let content_width = detail_route_inner_width(self, PRIMARY_ROUTE_MARGIN_START);
        let seed = stable_seed(artist.id.as_str());
        let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
        header.add_css_class("detail-showcase");
        header.add_css_class("artist-detail-showcase");
        mark_tiny_detail_showcase(&header, content_width);
        add_album_seed_gradient_class(&header, seed);

        let kind = localized_label(kind);
        kind.add_css_class("eyebrow");
        kind.set_xalign(0.0);
        let title = gtk::Label::new(Some(&artist.name));
        title.add_css_class("detail-title");
        title.set_xalign(0.0);
        title.set_wrap(true);
        title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        fit_detail_text(&title, &artist.name);
        let summary_label = gtk::Label::new(Some(summary));
        summary_label.add_css_class("muted");
        summary_label.set_xalign(0.0);
        header.append(&kind);
        header.append(&title);
        header.append(&summary_label);
        let resize_header = header.clone();
        let frame = detail_showcase_frame_with_back(self, header.upcast());
        let allocated_width = Cell::new(content_width);
        let root = width_allocation_owner(&frame, move |width| {
            if width > 1 && allocated_width.replace(width) != width {
                mark_tiny_detail_showcase(&resize_header, width);
            }
        });
        ArtistSubrouteHeaderProjection {
            root: root.upcast(),
            title,
            summary: summary_label,
        }
    }
}

fn set_artist_route_search(shell: &Shell, target: Option<ArtistRouteSearchTarget>) {
    match target {
        Some(target) => shell.set_route_search_with_focus(target.search, target.focus),
        None => shell.set_route_search(None),
    }
}

pub(crate) fn load_artist_overview(
    loaded: &Arc<Library>,
    artist_id: &ArtistId,
    music_folder_id: Option<&MusicFolderId>,
    track_settings: &LibraryListSettings,
    album_settings: &LibraryListSettings,
) -> Result<Option<ArtistOverview>, String> {
    let mut detail = loaded
        .artist_overview(artist_id, music_folder_id)
        .map_err(|error| error.to_string())?;
    if let Some(detail) = detail.as_mut() {
        detail.favorite_tracks = detail
            .favorite_tracks
            .sorted(
                track_settings.sort_key.track_sort(),
                track_settings.descending,
            )
            .map_err(|error| error.to_string())?;
        sort_albums(Arc::make_mut(&mut detail.albums), album_settings);
        sort_albums(Arc::make_mut(&mut detail.appears_on), album_settings);
    }
    Ok(detail)
}

pub(crate) fn load_artist_discography(
    loaded: &Arc<Library>,
    artist_id: &ArtistId,
    music_folder_id: Option<&MusicFolderId>,
    settings: &LibraryListSettings,
) -> Result<Option<ArtistDiscography>, String> {
    let mut detail = loaded
        .artist_discography(artist_id, music_folder_id)
        .map_err(|error| error.to_string())?;
    if let Some(detail) = detail.as_mut() {
        sort_albums(Arc::make_mut(&mut detail.albums), settings);
        sort_albums(Arc::make_mut(&mut detail.appears_on), settings);
    }
    Ok(detail)
}

pub(crate) fn load_artist_tracks(
    loaded: &Arc<Library>,
    artist_id: &ArtistId,
    music_folder_id: Option<&MusicFolderId>,
    settings: &LibraryListSettings,
) -> Result<Option<ArtistTracks>, String> {
    let mut detail = loaded
        .artist_track_detail(artist_id, music_folder_id)
        .map_err(|error| error.to_string())?;
    if let Some(detail) = detail.as_mut() {
        detail.tracks = detail
            .tracks
            .sorted(settings.sort_key.track_sort(), settings.descending)
            .map_err(|error| error.to_string())?;
    }
    Ok(detail)
}

fn section_heading(title: &str) -> gtk::Widget {
    let heading = localized_label(title);
    heading.add_css_class("section-heading");
    heading.set_xalign(0.0);
    heading.upcast()
}

fn artist_summary_text(album_count: u32, track_count: u32) -> String {
    format!(
        "{} / {}",
        album_count_text(u64::from(album_count)),
        track_count_text(u64::from(track_count))
    )
}

fn artist_count_button_with_label(icon_name: &str, text: &str) -> (gtk::Button, gtk::Label) {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("artist-count-button");
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let icon = gtk::Image::from_icon_name(icon_name);
    icon.set_pixel_size(ARTIST_COUNT_ICON_SIZE);
    icon.set_size_request(ARTIST_COUNT_ICON_SIZE, ARTIST_COUNT_ICON_SIZE);
    icon.set_halign(gtk::Align::Center);
    icon.set_valign(gtk::Align::Center);
    content.append(&icon);
    let label = gtk::Label::new(Some(text));
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&label);
    button.set_child(Some(&content));
    (button, label)
}

#[cfg(test)]
mod tests {
    use super::artist_summary_text;

    #[test]
    fn artist_summary_uses_loaded_relationship_counts() {
        assert_eq!(artist_summary_text(2, 3), "2 albums / 3 tracks");
    }
}
