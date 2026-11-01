use rufin_core::collection_context::CollectionTarget;
use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;
use library::{PlaylistKey, PlaylistRow, SmartPlaylistKey, SmartPlaylistRow};
use localization::{msgid, track_count_text};

use crate::CatalogUi;
use crate::{LibraryListKey, LibraryListSettings};
use gtk_widgets::artwork::presentation::stable_seed;
use gtk_widgets::format_duration_units;
use gtk_widgets::mounted_route::{MountedRoute, PageReread, route_view};
use rufin_core::view::Opened;

use super::collections::library_route_inset;
use super::detail_showcase::{
    CollectionDetailShowcase, DetailShowcaseView, collection_detail_showcase,
    detail_playback_controls, detail_radio_button,
};
use crate::route_layout::{
    DETAIL_SHOWCASE_METADATA_MIN_WIDTH, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
};
use gtk_media_menus::media_menus::present_smart_playlist_context_menu;
use gtk_widgets::library_fields::playlist_artwork;

const PLAYLIST_DETAIL_COMPACT_WIDTH: i32 = 760;
const PLAYLIST_DETAIL_TINY_COVER_SIZE: i32 = 150;
const PLAYLIST_DETAIL_WIDE_COVER_SIZE: i32 = 208;

#[derive(Clone, PartialEq)]
enum PlaylistDetailOwner {
    Saved {
        key: PlaylistKey,
        summary: PlaylistRow,
    },
    Smart {
        key: SmartPlaylistKey,
        /// When the page opened the playlist: its rows and totals stay as of then.
        now: i64,
        summary: SmartPlaylistRow,
    },
}

impl PlaylistDetailOwner {
    fn source_url(&self, catalog: &CatalogUi) -> Option<String> {
        let Self::Saved { summary, .. } = self else {
            return None;
        };
        let source = catalog
            .source
            .configuration(&sources::SourceId::new(summary.source_id.as_deref()?))?;
        source.detail_web_url("playlist", &summary.object_id).ok()
    }

    fn source_label(&self, catalog: &CatalogUi) -> (&'static str, String) {
        let source_id = match self {
            Self::Saved { summary, .. } => summary.source_id.as_deref(),
            Self::Smart { .. } => None,
        };
        (catalog.source_label)(source_id)
    }

    fn key(&self) -> LibraryListKey {
        match self {
            Self::Saved { .. } => LibraryListKey::PlaylistTracks,
            Self::Smart { .. } => LibraryListKey::SmartPlaylistTracks,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Saved { summary, .. } => &summary.name,
            Self::Smart { summary, .. } => &summary.name,
        }
    }

    fn seed(&self) -> u32 {
        match self {
            Self::Saved { summary, .. } => stable_seed(&summary.object_id),
            Self::Smart { summary, .. } => stable_seed(&summary.object_id),
        }
    }

    fn track_count(&self) -> i64 {
        match self {
            Self::Saved { summary, .. } => summary.track_count,
            Self::Smart { summary, .. } => summary.track_count,
        }
    }

    fn duration_millis(&self) -> i64 {
        match self {
            Self::Saved { summary, .. } => summary.duration_millis,
            Self::Smart { summary, .. } => summary.duration_millis,
        }
    }

    fn artwork(&self) -> Vec<ArtworkBinding> {
        match self {
            Self::Saved { summary, .. } => playlist_artwork(summary),
            Self::Smart { summary, .. } => summary
                .artwork_binding
                .iter()
                .map(|binding| ArtworkBinding::opaque(binding))
                .collect(),
        }
    }

    fn saved_key(&self) -> Option<PlaylistKey> {
        match self {
            Self::Saved { key, .. } => Some(*key),
            Self::Smart { .. } => None,
        }
    }

    fn target(&self) -> CollectionTarget {
        match self {
            Self::Saved { key, .. } => CollectionTarget::Playlist(*key),
            Self::Smart { key, .. } => CollectionTarget::SmartPlaylist(*key),
        }
    }

    /// The playlist as a page read again shows it; `None` when the read lacks it.
    fn read_from(&self, header: library::Header) -> Option<Self> {
        Some(match (self, header) {
            (Self::Saved { key, .. }, library::Header::Playlist(summary)) => Self::Saved {
                key: *key,
                summary: *summary,
            },
            (Self::Smart { key, now, .. }, library::Header::Smart(summary)) => Self::Smart {
                key: *key,
                now: *now,
                summary: *summary,
            },
            _ => return None,
        })
    }

    fn install_context(
        &self,
        shell: &Rc<CatalogUi>,
        target: &gtk::Widget,
        play: CollectionPlay,
        position: Option<(f64, f64)>,
    ) {
        match self {
            Self::Saved { summary, .. } => {
                (shell.playlist_context_menu)(target, summary.clone(), Some(play), position)
            }
            Self::Smart { summary, .. } => present_smart_playlist_context_menu(
                target,
                &shell.media_menus,
                summary.clone(),
                Some(play),
                position,
            ),
        }
    }
}

impl CatalogUi {
    pub fn smart_playlist_detail_route(
        self: &Rc<Self>,
        key: SmartPlaylistKey,
        mut read: Opened,
    ) -> MountedRoute {
        let (false, Some(library::Header::Smart(summary))) =
            (read.view.is_missing(), read.header.take())
        else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                msgid("Smart Playlist"),
                msgid("This isn't available"),
            ));
        };
        (self.update_smart_playlist_pin)(&summary);
        let Some(library::Collection::Smart { now, .. }) = read
            .view
            .members()
            .map(|members| members.collection.clone())
        else {
            unreachable!("Smart Playlist pages read their playlist")
        };
        let view = route_view(&read);
        let owner = PlaylistDetailOwner::Smart {
            key,
            now,
            summary: *summary,
        };
        let list_key = owner.key();
        let model =
            crate::track_model::TrackCollectionModel::new(self.runtime.clone(), &view, read);
        let play_model = model.clone();
        let play_queue = self.playback.clone();
        let play_context = owner.target().context_id();
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            play_model.play_source(
                play_queue.clone(),
                placement,
                play_context.clone(),
                shuffled,
            );
        });
        let (tracks_widget, tracks, toolbar) = self.scrolling_track_projection(
            model,
            list_key,
            "smart-playlist-detail",
            owner.target().context_id(),
        );
        let item_navigation = tracks.item_navigation();
        let apply_tracks = tracks.clone();
        let apply_toolbar = toolbar.clone();
        let apply = Rc::new(move |settings: &LibraryListSettings| {
            apply_tracks.apply_library_list_settings(list_key, settings);
            apply_toolbar.apply(list_key, settings);
        }) as Rc<dyn Fn(&LibraryListSettings)>;
        let search = tracks.search();
        let layout_cycle = toolbar.layout_cycle();
        let initial_demand = {
            let tracks = tracks.clone();
            Rc::new(move || tracks.resume_initial_demand()) as Rc<dyn Fn()>
        };
        let reread_tracks = tracks.clone();
        let shell = Rc::clone(self);
        self.shared_playlist_detail_route(
            owner,
            tracks_widget,
            item_navigation,
            apply,
            move |apply_owner| {
                let reread = reread_tracks.page_reread(
                    &shell,
                    "mounted Smart Playlist route",
                    &view,
                    |_| {},
                    move |header| apply_owner(header),
                );
                let refresh = Rc::clone(&reread);
                (
                    reread,
                    Rc::new(move || refresh.refresh(reread_tracks.live_window())),
                )
            },
            search,
            layout_cycle,
            initial_demand,
            play,
        )
        .with_download_change(tracks.download_change())
    }

    pub fn playlist_detail_route(
        self: &Rc<Self>,
        key: PlaylistKey,
        mut read: Opened,
    ) -> MountedRoute {
        let (false, Some(library::Header::Playlist(summary))) =
            (read.view.is_missing(), read.header.take())
        else {
            return MountedRoute::static_widget(crate::route_layout::placeholder_view(
                msgid("Playlist"),
                msgid("This isn't available"),
            ));
        };
        let view = route_view(&read);
        let owner = PlaylistDetailOwner::Saved {
            key,
            summary: *summary,
        };
        let entries = Rc::new(self.playlist_entries_view(
            key,
            owner.name().to_string(),
            matches!(&owner, PlaylistDetailOwner::Saved { summary, .. } if summary.writable),
            &view,
            read,
        ));
        let item_navigation = entries.item_navigation();
        let tracks_widget = entries.widget();
        let apply_entries = Rc::clone(&entries);
        let apply = Rc::new(move |settings: &LibraryListSettings| {
            apply_entries.apply_library_list_settings(LibraryListKey::PlaylistTracks, settings);
        }) as Rc<dyn Fn(&LibraryListSettings)>;
        let search = entries.search();
        let layout_cycle = entries.layout_cycle();
        let initial_demand = {
            let entries = Rc::clone(&entries);
            Rc::new(move || entries.resume_initial_demand()) as Rc<dyn Fn()>
        };
        let play_queue = self.playback.clone();
        let download_entries = Rc::clone(&entries);
        let reread_entries = Rc::clone(&entries);
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            entries.play(play_queue.clone(), placement, shuffled);
        });
        let shell = Rc::clone(self);
        let reread_search = search.clone();
        self.shared_playlist_detail_route(
            owner,
            tracks_widget,
            item_navigation,
            apply,
            move |apply_owner| {
                let apply_entries = Rc::clone(&reread_entries);
                let reread = PageReread::new(
                    shell.runtime.clone(),
                    "mounted Playlist route",
                    &view,
                    crate::routes::list_inputs(&reread_search),
                    move |mut read, previous| {
                        let header = read.header.take();
                        if !apply_entries.replace_prepared(previous, read) {
                            return false;
                        }
                        apply_owner(header);
                        true
                    },
                );
                let requested = RefCell::new(reread.view().options().filter.clone());
                let search_reread = Rc::downgrade(&reread);
                reread_search.connect_search_changed(move |search| {
                    let filter = search.text().trim().to_string();
                    if *requested.borrow() == filter {
                        return;
                    }
                    requested.replace(filter);
                    if let Some(reread) = search_reread.upgrade() {
                        reread.reread();
                    }
                });
                let refresh_reread = Rc::downgrade(&reread);
                let refresh = Rc::new(move || {
                    if let Some(reread) = refresh_reread.upgrade() {
                        reread.refresh(reread_entries.live_window());
                    }
                }) as Rc<dyn Fn()>;
                (reread, refresh)
            },
            search,
            layout_cycle,
            initial_demand,
            play,
        )
        .with_download_change(Rc::new(move |event| {
            let downloads::DownloadEvent::Changed {
                media_uri,
                downloaded,
            } = event
            else {
                return;
            };
            let (media_uri, downloaded) = (media_uri.as_str(), *downloaded);
            download_entries.update_downloaded(media_uri, downloaded);
        }))
    }

    fn shared_playlist_detail_route(
        self: &Rc<Self>,
        owner: PlaylistDetailOwner,
        tracks_widget: gtk::Widget,
        item_navigation: gtk_widgets::mounted_route::MountedRouteItemNavigation,
        apply_list_settings: Rc<dyn Fn(&LibraryListSettings)>,
        reread: impl FnOnce(Rc<dyn Fn(Option<library::Header>)>) -> (Rc<PageReread>, Rc<dyn Fn()>),
        search: gtk::SearchEntry,
        layout_cycle: gtk_widgets::mounted_route::MountedRouteCommand,
        initial_demand: Rc<dyn Fn()>,
        play: CollectionPlay,
    ) -> MountedRoute {
        let key = owner.key();
        let owner_state = Rc::new(RefCell::new(owner.clone()));
        let wrapper = gtk::Box::new(
            gtk::Orientation::Vertical,
            if owner.saved_key().is_some() { 20 } else { 18 },
        );
        wrapper.add_css_class("route-content");
        wrapper.set_hexpand(true);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);

        let width = crate::route_layout::detail_route_inner_width_for_viewport(
            (self.route_width)(),
            PRIMARY_ROUTE_MARGIN_START,
        );
        let artwork = owner.artwork();
        let cover = self.artwork.cover_group_projection_for_artwork(
            &artwork,
            playlist_cover_size(width),
            playlist_cover_size(i32::MAX),
            false,
        );
        cover.widget().add_css_class("playlist-detail-cover");
        let showcase_view = DetailShowcaseView::new(
            "playlist-detail-showcase",
            owner.seed(),
            "Playlist",
            true,
            owner.name(),
        );
        self.append_playlist_detail_kind_controls(&showcase_view, &owner);
        showcase_view.replace_summary(&[
            (
                "rufin-tracks-symbolic",
                track_count_text(owner.track_count().max(0) as u64),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units((owner.duration_millis().max(0) / 1_000) as u32),
            ),
        ]);
        let (source_icon, source_name) = owner.source_label(self);
        showcase_view.set_source_summary(
            source_icon,
            &source_name,
            owner.source_url(self).as_deref(),
        );
        let actions = showcase_view.actions();
        actions.set_halign(gtk::Align::Start);
        let controls =
            detail_playback_controls(&actions, msgid("Play playlist"), None, Rc::clone(&play));
        if let PlaylistDetailOwner::Saved { summary, .. } = &owner
            && summary.writable
        {
            let drop = gtk::DropTarget::new(
                glib::BoxedAnyObject::static_type(),
                gtk::gdk::DragAction::COPY,
            );
            let drop_owner = Rc::clone(&owner_state);
            let playlist = summary.playlist_key;
            drop.set_preload(true);
            drop.connect_notify_local(Some("value"), move |drop, _| {
                let Some(value) = drop.value() else { return };
                if matches!(gtk_widgets::media_drag::media_drag_source(&value), Some(gtk_widgets::media_drag::MediaDragSource::PlaylistEntries(selection)) if selection.playlist == playlist && selection.writable && selection.count == 1) {
                    drop.reject();
                }
            });
            let drop_menus = Rc::downgrade(&self.media_menus);
            drop.connect_drop(move |_, value, _, _| {
                let Some(menus) = drop_menus.upgrade() else {
                    return false;
                };
                let Some(source) = gtk_widgets::media_drag::media_drag_source(value) else {
                    return false;
                };
                let PlaylistDetailOwner::Saved { summary, .. } = drop_owner.borrow().clone() else {
                    return false;
                };
                gtk_media_menus::media_menus::add_drag_to_playlist(
                    &menus,
                    summary.playlist_key,
                    summary.name,
                    source,
                )
            });
            wrapper.add_css_class("media-drop-target");
            wrapper.add_controller(drop);
        }

        let menu_shell = Rc::clone(self);
        let menu_owner = Rc::clone(&owner_state);
        let menu_play = Rc::clone(&play);
        let context_menu = Rc::new(move |target: &gtk::Widget, position| {
            menu_owner.borrow().install_context(
                &menu_shell,
                target,
                Rc::clone(&menu_play),
                position,
            );
        });
        let showcase = collection_detail_showcase(CollectionDetailShowcase {
            view: showcase_view.clone(),
            initial_width: width,
            compact_spacing: 20,
            wide_spacing: 28,
            cover: cover.clone(),
            cover_controls: controls,
            context_menu: Some(context_menu),
            open_cover: {
                let present = Rc::clone(&self.present_full_artwork);
                let owner = Rc::clone(&owner_state);
                Rc::new(move || {
                    let artwork = owner.borrow().artwork();
                    present(artwork);
                })
            },
        });
        wrapper.append(&library_route_inset(showcase));
        wrapper.append(&tracks_widget);

        let apply_owner = {
            let shell = Rc::downgrade(self);
            let owner = Rc::clone(&owner_state);
            let showcase = showcase_view.clone();
            let cover = cover.clone();
            Rc::new(move |header: Option<library::Header>| {
                let Some(shell) = shell.upgrade() else { return };
                let Some(next) = header.and_then(|header| owner.borrow().read_from(header)) else {
                    return;
                };
                if *owner.borrow() == next {
                    return;
                }
                showcase.set_title(next.name());
                showcase.replace_summary(&[
                    (
                        "rufin-tracks-symbolic",
                        track_count_text(next.track_count().max(0) as u64),
                    ),
                    (
                        "rufin-preferences-system-time-symbolic",
                        format_duration_units((next.duration_millis().max(0) / 1_000) as u32),
                    ),
                ]);
                let (source_icon, source_name) = next.source_label(&shell);
                showcase.set_source_summary(
                    source_icon,
                    &source_name,
                    next.source_url(&shell).as_deref(),
                );
                cover.replace(&shell.artwork, &next.artwork());
                shell.append_playlist_detail_kind_controls(&showcase, &next);
                owner.replace(next);
            }) as Rc<dyn Fn(Option<library::Header>)>
        };
        let (reread, refresh) = reread(apply_owner);
        let states = Rc::clone(reread.route_view());
        let resume = {
            let shell = Rc::downgrade(self);
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                apply_list_settings(&settings);
                reread.resume(&shell.settings.current.borrow());
            })
        };

        let download_owner = Rc::clone(&owner_state);
        let downloads = crate::collection_download_change(move |target, downloaded| {
            let mut owner = download_owner.borrow_mut();
            if *target != owner.target() {
                return;
            }
            match &mut *owner {
                PlaylistDetailOwner::Saved { summary, .. } => {
                    summary.downloaded_count = if downloaded { summary.track_count } else { 0 };
                }
                PlaylistDetailOwner::Smart { summary, .. } => {
                    summary.downloaded_count = if downloaded { summary.track_count } else { 0 };
                }
            }
        });
        MountedRoute::new(wrapper.upcast(), resume)
            .with_download_change(downloads)
            .with_search(search)
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(item_navigation)
            .with_initial_demand(initial_demand)
            .with_catalog_refresh(refresh)
            .with_states(&states)
    }

    fn append_playlist_detail_kind_controls(
        self: &Rc<Self>,
        showcase: &DetailShowcaseView,
        owner: &PlaylistDetailOwner,
    ) {
        showcase.clear_kind_controls();
        if let PlaylistDetailOwner::Saved { key, .. } = owner {
            let radio = detail_radio_button();
            let controller = self.playback.clone();
            let key = *key;
            radio.connect_clicked(move |_| {
                controller.play_radio(rufin_core::playback::RadioPlayRequest::now(
                    library::RadioSeed::Playlist(key),
                ));
            });
            showcase.append_kind_control(&radio);
        }
        if let PlaylistDetailOwner::Saved { summary, .. } = owner {
            showcase.set_genres(
                self,
                summary
                    .genres
                    .iter()
                    .map(|genre| (genre.name.as_str(), genre.genre_key)),
            );
        }
    }
}

pub fn playlist_cover_size(width: i32) -> i32 {
    if width < DETAIL_SHOWCASE_METADATA_MIN_WIDTH {
        width.clamp(96, PLAYLIST_DETAIL_TINY_COVER_SIZE)
    } else if playlist_detail_compact_for_width(width) {
        PLAYLIST_DETAIL_TINY_COVER_SIZE
            + ((width - DETAIL_SHOWCASE_METADATA_MIN_WIDTH)
                * (PLAYLIST_DETAIL_WIDE_COVER_SIZE - PLAYLIST_DETAIL_TINY_COVER_SIZE)
                / (PLAYLIST_DETAIL_COMPACT_WIDTH - DETAIL_SHOWCASE_METADATA_MIN_WIDTH))
    } else {
        PLAYLIST_DETAIL_WIDE_COVER_SIZE
    }
}

pub fn playlist_detail_compact_for_width(width: i32) -> bool {
    width < PLAYLIST_DETAIL_COMPACT_WIDTH
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playlist_cover_respects_checkpoint_breakpoints() {
        assert_eq!(playlist_cover_size(359), PLAYLIST_DETAIL_TINY_COVER_SIZE);
        assert_eq!(playlist_cover_size(420), PLAYLIST_DETAIL_TINY_COVER_SIZE);
        assert_eq!(playlist_cover_size(760), PLAYLIST_DETAIL_WIDE_COVER_SIZE);
    }

    #[test]
    fn mounted_detail_covers_track_width_without_resize_jumps() {
        let media_limit = crate::route_layout::detail_showcase_cover_size(i32::MAX);
        let collection_limit = playlist_cover_size(i32::MAX);
        let mut previous_media = crate::route_layout::detail_showcase_cover_size(96);
        let mut previous_collection = playlist_cover_size(96);
        for width in 97..=900 {
            let media = crate::route_layout::detail_showcase_cover_size(width);
            let collection = playlist_cover_size(width);
            assert!(media >= previous_media && media - previous_media <= 1);
            assert!(collection >= previous_collection && collection - previous_collection <= 1);
            assert!(media <= media_limit);
            assert!(collection <= collection_limit);
            assert!(media <= width);
            assert!(collection <= width);
            previous_media = media;
            previous_collection = collection;
        }
    }

    #[test]
    fn provider_playlist_cover_is_not_nested_inside_representative_mosaic() {
        let owner = PlaylistDetailOwner::Saved {
            key: PlaylistKey::from_raw(1),
            summary: PlaylistRow {
                source_owned: true,
                writable: true,
                metadata_writable: true,
                playlist_key: PlaylistKey::from_raw(1),
                source_key: Some(library::SourceKey::from_raw(1)),
                source_id: Some("source".into()),
                object_id: "playlist".to_string(),
                name: "Playlist".to_string(),
                artwork_binding: Some(vec![1]),
                track_count: 4,
                duration_millis: 1,
                downloaded_count: 0,
                genres: Vec::new(),
            },
        };

        assert_eq!(owner.artwork().len(), 1);
    }
}

use gtk_media_menus::media_menus::CollectionPlay;
