use rufin_core::view::OpenOptions;
use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use artwork::ArtworkBinding;
use library::{ArtistKey, ArtistView, RadioSeed};
use localization::{album_count_text, msgid, track_count_text};
use rufin_core::playback::RadioPlayRequest;

use crate::favorites::{
    favorite_button_is_active, favorite_icon_button, set_favorite_button_active,
};
use crate::library::CatalogUi;
use crate::library::LibraryListKey;
use crate::library::routes::PageReads;
use crate::widgets::artwork::presentation::stable_seed;
use crate::widgets::controls::{ActionButtonVariant, configure_action_button};
use crate::widgets::layout::width_allocation_owner;
use crate::widgets::localization::localized_label;
use crate::widgets::mounted_route::MountedRoute;
use crate::widgets::view_model::Rows;
use rufin_core::view::Opened;

use super::artist_releases::{ArtistReleaseProjections, ArtistReleaseRoutePreamble};
use super::detail_showcase::{
    DetailShowcaseView, MediaCoverProjection, MediaShowcase, detail_playback_controls,
    detail_radio_button, detail_showcase_frame, media_cover_projection, media_showcase,
};
use super::routes::SearchableTrackOptions;
use crate::library::route_layout::PRIMARY_ROUTE_MARGIN_START;
use crate::menus::media_menus::present_row_menu;
use rufin_core::route::Route;
use rufin_core::settings::presentation::ARTIST_RELEASE_TITLES;

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

/// An artist page's header, when the read has one and the artist is there.
fn artist_header(read: &mut Opened) -> Option<ArtistView> {
    match (read.view.is_missing(), read.header.take()) {
        (false, Some(library::Header::Artist(artist))) => Some(*artist),
        _ => None,
    }
}

impl CatalogUi {
    pub fn artist_detail_view(
        self: &Rc<Self>,
        album_artist: bool,
        mut read: Opened,
    ) -> MountedRoute {
        let (Some(summary), Some(favorite_rows), true) = (
            artist_header(&mut read),
            read.favorites.take(),
            read.view.favorites().is_some(),
        ) else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                "Artist",
                msgid("No music from this artist yet"),
            ));
        };
        let artist = summary.row.artist_key;
        let header = artist_detail_header_restored(self, &summary, artist, album_artist);
        let favorite_count = read.view.favorites_len().unwrap_or(0);
        let favorite_model =
            Rows::favorites(self.products.runtime.clone(), &read.view, favorite_rows);
        let favorite = self.track_projection(
            favorite_model.clone(),
            LibraryListKey::ArtistTracks,
            SearchableTrackOptions {
                content_inset: 0,
                fixed_layout: Some(crate::library::LibraryLayout::Row),
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
        crate::widgets::layout::configure_fill_width_clip(
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
                empty: crate::library::route_layout::route_empty_view(msgid(
                    "No music from this artist yet",
                )),
            },
            ARTIST_RELEASE_TITLES,
            &mut read,
        ));
        // The releases take each read; a whole-page read also has the header and the favorite
        // tracks, which a release reread leaves as they are.
        let apply = {
            let shell = Rc::downgrade(self);
            let favorite_model = favorite_model.clone();
            let favorite_scroller = favorite_scroller.clone();
            let releases = Rc::downgrade(&releases);
            let header = header.clone();
            move |mut read: Opened| {
                let (Some(shell), Some(releases)) = (shell.upgrade(), releases.upgrade()) else {
                    return;
                };
                releases.take(&read);
                if read.favorites.is_some()
                    && let Some(summary) =
                        artist_header(&mut read).filter(|_| read.view.favorites().is_some())
                {
                    favorite_model.take(&mut read);
                    header.replace(&shell, summary);
                    let present = !favorite_model.is_empty();
                    releases.set_favorite_present(present);
                    let height = 30 + favorite_model.len().min(4) as i32 * 64;
                    favorite_scroller.set_max_content_height(-1);
                    favorite_scroller.set_min_content_height(height);
                    favorite_scroller.set_max_content_height(height);
                }
                releases.apply_library_list_settings(
                    &shell
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::ArtistAlbums),
                );
                shell.reapply_current_track();
            }
        };
        let favorite_search = favorite.search();
        let filters = Rc::downgrade(&releases);
        let reads = PageReads::new(
            &releases.sparse,
            move |options| OpenOptions {
                filter: favorite_search.text().trim().to_string(),
                release_filters: filters.upgrade().map(|releases| releases.filters()),
                ..options.clone()
            },
            apply,
        );
        crate::library::routes::connect_list_search(&favorite.search(), &reads);
        connect_artist_release_requests(self, &releases, &reads);
        let resume = {
            let shell = Rc::downgrade(self);
            let favorite = favorite.clone();
            let releases = Rc::clone(&releases);
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let track_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistTracks);
                favorite.apply_library_list_settings(LibraryListKey::ArtistTracks, &track_settings);
                favorite_toolbar.apply(LibraryListKey::ArtistTracks, &track_settings);
                let album_settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                releases.apply_library_list_settings(&album_settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        MountedRoute::new(releases.widget(), resume)
            .with_rows(&releases.sparse)
            .with_rows(&favorite_model)
            .with_states(header.states())
            .with_header(header.shown(self))
            .with_item_scroll({
                let releases = Rc::downgrade(&releases);
                Rc::new(move || releases.upgrade().map(|releases| releases.item_scroll()))
            })
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
                Rc::new(move || releases.sparse.resume_initial_demand())
            })
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
    }

    pub fn artist_tracks_view(
        self: &Rc<Self>,
        album_artist: bool,
        mut read: Opened,
    ) -> MountedRoute {
        let Some(summary) = artist_header(&mut read) else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                msgid("Tracks"),
                msgid("This isn't available"),
            ));
        };
        self.artist_track_surface_restored(summary, read, album_artist, false)
    }

    pub fn artist_favorite_tracks_view(
        self: &Rc<Self>,
        album_artist: bool,
        mut read: Opened,
    ) -> MountedRoute {
        let Some(summary) = artist_header(&mut read) else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                msgid("Favorite tracks"),
                msgid("No favorites yet"),
            ));
        };
        self.artist_track_surface_restored(summary, read, album_artist, true)
    }

    pub fn artist_discography_view(self: &Rc<Self>, mut read: Opened) -> MountedRoute {
        let Some(summary) = artist_header(&mut read) else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                msgid("Discography"),
                msgid("No albums from this artist yet"),
            ));
        };
        let releases = Rc::new(ArtistReleaseProjections::new(
            self,
            ArtistReleaseRoutePreamble {
                header: artist_subroute_header(self, &summary, msgid("Discography")),
                favorite: None,
                favorite_present: false,
                empty: crate::library::route_layout::route_empty_view(msgid(
                    "No albums from this artist yet",
                )),
            },
            ARTIST_RELEASE_TITLES,
            &mut read,
        ));
        let apply = {
            let shell = Rc::downgrade(self);
            let releases = Rc::downgrade(&releases);
            move |read: Opened| {
                let (Some(shell), Some(releases)) = (shell.upgrade(), releases.upgrade()) else {
                    return;
                };
                releases.take(&read);
                shell.reapply_current_track();
            }
        };
        let filters = Rc::downgrade(&releases);
        let reads = PageReads::new(
            &releases.sparse,
            move |options| OpenOptions {
                release_filters: filters.upgrade().map(|releases| releases.filters()),
                ..options.clone()
            },
            apply,
        );
        connect_artist_release_requests(self, &releases, &reads);
        let resume = {
            let releases = Rc::clone(&releases);
            let shell = Rc::downgrade(self);
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums);
                releases.apply_library_list_settings(&settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        let refresh = {
            let releases = Rc::clone(&releases);
            let shell = Rc::downgrade(self);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                releases.apply_library_list_settings(
                    &shell
                        .settings
                        .current
                        .borrow()
                        .library_list(LibraryListKey::ArtistAlbums),
                );
                reads.refresh();
            }) as Rc<dyn Fn()>
        };
        MountedRoute::new(releases.widget(), resume)
            .with_rows(&releases.sparse)
            .with_item_scroll({
                let releases = Rc::downgrade(&releases);
                Rc::new(move || releases.upgrade().map(|releases| releases.item_scroll()))
            })
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
                Rc::new(move || releases.sparse.resume_initial_demand())
            })
            .with_catalog_refresh(refresh)
    }

    fn artist_track_surface_restored(
        self: &Rc<Self>,
        summary: ArtistView,
        mut read: Opened,
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
        let rows = Rows::over_view(self.products.runtime.clone(), &mut read);
        let (tracks_widget, tracks, toolbar) =
            self.scrolling_track_projection(rows.clone(), key, context);
        let root = crate::library::route_layout::detail_route_wrapper(18);
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
        let reads = tracks.page_reads(|_| {}, |_| {});
        let resume = {
            let shell = Rc::downgrade(self);
            let projection = tracks.clone();
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else { return };
                let settings = shell.settings.current.borrow().library_list(key);
                projection.apply_library_list_settings(key, &settings);
                toolbar.apply(key, &settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        MountedRoute::new(root.upcast(), resume)
            .with_rows(&rows)
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
            .with_search(tracks.search())
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(tracks.item_navigation())
            .with_initial_demand(Rc::new(move || rows.resume_initial_demand()))
    }
}

/// Release searches read the releases again.
fn connect_artist_release_requests(
    shell: &Rc<CatalogUi>,
    releases: &Rc<ArtistReleaseProjections>,
    reads: &PageReads,
) {
    for index in 0..ARTIST_RELEASE_TITLES.len() {
        let Some(search) = releases.section_search(index) else {
            continue;
        };
        let weak_shell = Rc::downgrade(shell);
        let releases = Rc::downgrade(releases);
        let reread = reads.weak(PageReads::reread_releases);
        search.connect_search_changed(move |_| {
            let (Some(shell), Some(releases)) = (weak_shell.upgrade(), releases.upgrade()) else {
                return;
            };
            releases.apply_library_list_settings(
                &shell
                    .settings
                    .current
                    .borrow()
                    .library_list(LibraryListKey::ArtistAlbums),
            );
            reread();
        });
    }
}

#[derive(Clone)]
struct ArtistDetailHeaderProjection {
    album_artist: bool,
    root: gtk::Widget,
    current: Rc<RefCell<ArtistView>>,
    hearts: Rc<[gtk::Button]>,
    showcase: DetailShowcaseView,
    album_count: gtk::Label,
    track_count: gtk::Label,
    cover: MediaCoverProjection,
}

impl ArtistDetailHeaderProjection {
    fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    fn replace(&self, shell: &Rc<CatalogUi>, artist: ArtistView) {
        self.showcase.set_title(&artist.row.name);
        self.album_count.set_text(&album_count_text(
            artist.row.summary.album_count.max(0) as u64
        ));
        self.track_count.set_text(&track_count_text(
            artist.row.summary.track_count.max(0) as u64
        ));
        self.cover.replace(
            shell,
            artist
                .row
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
        for heart in self.hearts.iter() {
            set_favorite_button_active(heart, artist.row.state.favorite);
        }
        self.current.replace(artist);
    }

    /// Shows the artist as a header read again shows it.
    fn shown(&self, shell: &Rc<CatalogUi>) -> crate::widgets::mounted_route::MountedHeader {
        let header = self.clone();
        let shell = Rc::downgrade(shell);
        Rc::new(move |read| {
            if let (Some(shell), Some(library::Header::Artist(artist))) = (shell.upgrade(), read) {
                header.replace(&shell, *artist);
            }
        })
    }

    /// Shows a committed favorite of the artist in its hearts.
    fn states(&self) -> crate::widgets::mounted_route::MountedStates {
        let current = Rc::clone(&self.current);
        let hearts = Rc::clone(&self.hearts);
        Rc::new(move |changes, _| {
            use library::ShowsState;
            let mut artist = current.borrow_mut();
            for change in changes {
                if let Some(state) = artist.row.state_mut(change) {
                    change.show_in(state);
                    for heart in hearts.iter() {
                        set_favorite_button_active(heart, change.favorite);
                    }
                }
            }
        })
    }
}

fn artist_detail_header_restored(
    shell: &Rc<CatalogUi>,
    artist: &ArtistView,
    artist_key: ArtistKey,
    album_artist: bool,
) -> ArtistDetailHeaderProjection {
    let current = Rc::new(RefCell::new(artist.clone()));
    let width = crate::library::route_layout::detail_route_inner_width_for_viewport(
        shell.route_width(),
        PRIMARY_ROUTE_MARGIN_START,
    );
    let cover = media_cover_projection(
        shell,
        artist
            .row
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        crate::library::route_layout::detail_showcase_cover_size(width),
        "artist-detail-cover",
    );
    let showcase = DetailShowcaseView::new(
        "artist-detail-showcase",
        stable_seed(&artist.row.object_id),
        "Artist",
        true,
        &artist.row.name,
    );
    let radio = detail_radio_button();
    let radio_controller = shell.products.playback.clone();
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
        &album_count_text(artist.row.summary.album_count.max(0) as u64),
    );
    let album_shell = Rc::clone(shell);
    let album_uri = artist.row.media_uri.clone();
    albums.connect_clicked(move |_| {
        album_shell.open_route(artist_discography_route(album_uri.clone(), album_artist))
    });
    counts.append(&albums);
    let (tracks, track_count) = artist_count_button(
        "rufin-tracks-symbolic",
        &track_count_text(artist.row.summary.track_count.max(0) as u64),
    );
    let track_shell = Rc::clone(shell);
    let track_uri = artist.row.media_uri.clone();
    tracks.connect_clicked(move |_| {
        track_shell.open_route(artist_tracks_route(track_uri.clone(), album_artist))
    });
    counts.append(&tracks);
    showcase.append_detail(&counts);

    let actions = showcase.actions();
    actions.add_css_class("artist-detail-actions");
    actions.set_halign(gtk::Align::Start);
    let target = crate::widgets::selection::PlayTarget::artist(&artist.row, album_artist);
    let play_shell = Rc::clone(shell);
    let play: CollectionPlay = Rc::new(move |placement, shuffled| {
        crate::menus::media_menus::play_target(&play_shell.products, &target, placement, shuffled);
    });
    let controls = detail_playback_controls(
        &actions,
        msgid("Play artist"),
        Some(artist.row.state.favorite),
        Rc::clone(&play),
    );
    let favorite = favorite_icon_button("Favorite");
    configure_action_button(&favorite, ActionButtonVariant::DetailFavorite);
    set_favorite_button_active(&favorite, artist.row.state.favorite);
    actions.append(&favorite);
    let hover_favorite = controls
        .favorite
        .as_ref()
        .expect("artist detail has a Favorite cover control")
        .clone();
    let favorite_media_uri = artist.row.media_uri.clone();
    let hearts: Rc<[gtk::Button]> = Rc::new([favorite.clone(), hover_favorite.clone()]);
    for button in [favorite, hover_favorite] {
        let favorite_shell = Rc::clone(shell);
        let favorite_media_uri = favorite_media_uri.clone();
        button.connect_clicked(move |button| {
            favorite_shell.set_favorite(
                library::FavoriteTarget::Artist(favorite_media_uri.clone()),
                !favorite_button_is_active(button),
            );
        });
    }
    let menu_shell = Rc::clone(shell);
    let menu_artist = Rc::clone(&current);
    let menu_play = Rc::clone(&play);
    let context_menu = Rc::new(move |target: &gtk::Widget, position| {
        let artist = menu_artist.borrow().row.clone();
        {
            present_row_menu(
                target,
                menu_shell.shell(),
                library::Row::Artist(artist),
                Some(Rc::clone(&menu_play)),
                position,
            );
        }
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
        hearts,
        showcase,
        album_count,
        track_count,
        cover,
    }
}

fn artist_subroute_header(
    shell: &Rc<CatalogUi>,
    artist: &ArtistView,
    kind_text: &str,
) -> gtk::Widget {
    let width = crate::library::route_layout::detail_route_inner_width_for_viewport(
        shell.route_width(),
        PRIMARY_ROUTE_MARGIN_START,
    );
    let header = gtk::Box::new(gtk::Orientation::Vertical, 8);
    header.add_css_class("detail-showcase");
    header.add_css_class("artist-detail-showcase");
    if width < 520 {
        header.add_css_class("detail-showcase-tiny");
    }
    crate::widgets::artwork::presentation::add_album_seed_gradient_class(
        &header,
        stable_seed(&artist.row.object_id),
    );
    let kind = localized_label(kind_text);
    kind.add_css_class("eyebrow");
    kind.set_xalign(0.0);
    let title = gtk::Label::new(Some(&artist.row.name));
    title.add_css_class("detail-title");
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    super::detail_showcase::fit_detail_text(&title, &artist.row.name);
    let summary = gtk::Label::new(Some(&format!(
        "{} / {}",
        album_count_text(artist.row.summary.album_count.max(0) as u64),
        track_count_text(artist.row.summary.track_count.max(0) as u64)
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
}

use crate::menus::media_menus::CollectionPlay;
