use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use library::{PlaylistKey, PlaylistRow, SmartPlaylistRow};
use localization::{msgid, track_count_text};
use rufin_core::artwork::ArtworkBinding;

use crate::library::CatalogUi;
use crate::library::routes::PageReads;
use crate::library::{LibraryListKey, LibraryListSettings};
use crate::widgets::mounted_route::MountedRoute;
use crate::widgets::view_model::Rows;
use rufin_core::settings::presentation::format_duration_units;
use rufin_core::view::Opened;

use super::collections::library_route_inset;
use super::detail_showcase::{
    DetailShowcaseView, ShowcaseCover, ShowcaseMount, ShowcaseShape, source_link,
};
use crate::library::route_layout::{
    PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, detail_showcase_cover_size,
};
use crate::menus::media_menus::present_row_menu;
use crate::widgets::library_fields::playlist_artwork;
use library::PlaylistListRow;

#[derive(Clone, PartialEq)]
enum PlaylistDetailOwner {
    Saved {
        key: PlaylistKey,
        summary: PlaylistRow,
    },
    Smart {
        summary: SmartPlaylistRow,
    },
}

impl PlaylistDetailOwner {
    /// The header it shows.
    fn header(&self) -> library::Header {
        match self {
            Self::Saved { summary, .. } => library::Header::Playlist(Box::new(summary.clone())),
            Self::Smart { summary, .. } => library::Header::Smart(Box::new(summary.clone())),
        }
    }

    fn source_url(&self, catalog: &CatalogUi) -> Option<String> {
        let Self::Saved { summary, .. } = self else {
            return None;
        };
        let source = catalog
            .products
            .source
            .configuration(&sources::SourceId::new(summary.source_id.as_deref()?))?;
        source.detail_web_url("playlist", &summary.object_id).ok()
    }

    fn source_label(&self, catalog: &CatalogUi) -> (&'static str, String) {
        let source_id = match self {
            Self::Saved { summary, .. } => summary.source_id.as_deref(),
            Self::Smart { .. } => None,
        };
        catalog.source_label(source_id)
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

    /// The playlist as a page read again shows it; `None` when the read lacks it.
    fn read_from(&self, header: library::Header) -> Option<Self> {
        Some(match (self, header) {
            (Self::Saved { key, .. }, library::Header::Playlist(summary)) => Self::Saved {
                key: *key,
                summary: *summary,
            },
            (Self::Smart { .. }, library::Header::Smart(summary)) => {
                Self::Smart { summary: *summary }
            }
            _ => return None,
        })
    }

    /// The row its menu and header buttons act on.
    fn row(&self) -> library::Row {
        library::Row::Playlist(match self {
            Self::Saved { summary, .. } => PlaylistListRow::Playlist(summary.clone()),
            Self::Smart { summary, .. } => PlaylistListRow::SmartPlaylist(summary.clone()),
        })
    }

    /// Its facts: tracks and duration.
    fn summary(&self) -> [(&'static str, String); 2] {
        [
            (
                "rufin-tracks-symbolic",
                track_count_text(self.track_count().max(0) as u64),
            ),
            (
                "rufin-preferences-system-time-symbolic",
                format_duration_units((self.duration_millis().max(0) / 1_000) as u32),
            ),
        ]
    }

    /// Its source at the end of the facts.
    fn source(&self, shell: &CatalogUi) -> gtk::Widget {
        let (icon, name) = self.source_label(shell);
        source_link(shell, icon, &name, self.source_url(shell))
    }

    /// A saved playlist's genres.
    fn genres(&self) -> Vec<(&str, library::GenreKey)> {
        match self {
            Self::Saved { summary, .. } => summary
                .genres
                .iter()
                .map(|genre| (genre.name.as_str(), genre.genre_key))
                .collect(),
            Self::Smart { .. } => Vec::new(),
        }
    }
}

impl CatalogUi {
    pub fn smart_playlist_detail_route(self: &Rc<Self>, mut read: Opened) -> MountedRoute {
        let (false, Some(library::Header::Smart(summary))) =
            (read.view.is_missing(), read.header.take())
        else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                msgid("Smart Playlist"),
                msgid("This isn't available"),
            ));
        };
        let owner = PlaylistDetailOwner::Smart { summary: *summary };
        let list_key = owner.key();
        let rows = Rows::over_view(self.products.runtime.clone(), &mut read);
        let play_rows = rows.clone();
        let play_queue = self.products.playback.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            play_rows.play_all(&play_queue, placement, shuffled);
        });
        let (tracks_widget, tracks, toolbar) =
            self.scrolling_track_projection(rows.clone(), list_key, "smart-playlist-detail");
        let item_navigation = tracks.item_navigation();
        let apply_tracks = tracks.clone();
        let apply = Rc::new(move |settings: &LibraryListSettings| {
            apply_tracks.apply_library_list_settings(list_key, settings);
        }) as Rc<dyn Fn(&LibraryListSettings)>;
        let search = tracks.search();
        let layout_cycle = toolbar.layout_cycle();
        self.shared_playlist_detail_route(
            owner,
            rows,
            tracks_widget,
            item_navigation,
            apply,
            move |apply_owner| tracks.page_reads(|_| {}, move |header| apply_owner(header)),
            search,
            layout_cycle,
            play,
        )
    }

    pub fn playlist_detail_route(
        self: &Rc<Self>,
        key: PlaylistKey,
        mut read: Opened,
    ) -> MountedRoute {
        let (false, Some(library::Header::Playlist(summary))) =
            (read.view.is_missing(), read.header.take())
        else {
            return MountedRoute::static_widget(crate::library::route_layout::placeholder_view(
                msgid("Playlist"),
                msgid("This isn't available"),
            ));
        };
        let owner = PlaylistDetailOwner::Saved {
            key,
            summary: *summary,
        };
        let rows = Rows::over_view(self.products.runtime.clone(), &mut read);
        let entries = Rc::new(self.playlist_entries_view(
            key,
            owner.name().to_string(),
            matches!(&owner, PlaylistDetailOwner::Saved { summary, .. } if summary.writable),
            rows.clone(),
        ));
        let item_navigation = entries.item_navigation();
        let tracks_widget = entries.widget();
        let apply_entries = Rc::clone(&entries);
        let apply = Rc::new(move |settings: &LibraryListSettings| {
            apply_entries.apply_library_list_settings(LibraryListKey::PlaylistTracks, settings);
        }) as Rc<dyn Fn(&LibraryListSettings)>;
        let search = entries.search();
        let layout_cycle = entries.layout_cycle();
        let play_queue = self.products.playback.clone();
        let reread_entries = Rc::clone(&entries);
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            entries.play(&play_queue, placement, shuffled);
        });
        let reread_search = search.clone();
        let reread_rows = rows.clone();
        self.shared_playlist_detail_route(
            owner,
            rows,
            tracks_widget,
            item_navigation,
            apply,
            move |apply_owner| {
                let reads = PageReads::new(
                    &reread_rows,
                    crate::library::routes::list_inputs(&reread_search),
                    move |mut read| {
                        reread_entries.show_empty();
                        apply_owner(read.header.take());
                    },
                );
                let requested = RefCell::new(
                    reread_rows
                        .view()
                        .map(|view| view.options().filter.clone())
                        .unwrap_or_default(),
                );
                let reread = reads.weak(PageReads::reread);
                reread_search.connect_search_changed(move |search| {
                    let filter = search.text().trim().to_string();
                    if *requested.borrow() == filter {
                        return;
                    }
                    requested.replace(filter);
                    reread();
                });
                reads
            },
            search,
            layout_cycle,
            play,
        )
    }

    fn shared_playlist_detail_route(
        self: &Rc<Self>,
        owner: PlaylistDetailOwner,
        rows: Rows<library::Row>,
        tracks_widget: gtk::Widget,
        item_navigation: crate::widgets::mounted_route::MountedRouteItemNavigation,
        apply_list_settings: Rc<dyn Fn(&LibraryListSettings)>,
        reads: impl FnOnce(Rc<dyn Fn(Option<library::Header>)>) -> PageReads,
        search: gtk::SearchEntry,
        layout_cycle: crate::widgets::mounted_route::MountedRouteCommand,
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

        let width = crate::library::route_layout::detail_route_inner_width_for_viewport(
            self.route_width(),
            PRIMARY_ROUTE_MARGIN_START,
        );
        let artwork = owner.artwork();
        let cover = self.artwork.cover_group_projection_for_artwork(
            &artwork,
            ShowcaseShape::Detail.cover_size(width),
            detail_showcase_cover_size(i32::MAX),
            false,
        );
        cover.widget().add_css_class("playlist-detail-cover");
        let showcase_view = DetailShowcaseView::new(owner.name());
        showcase_view.replace_summary(&owner.summary());
        showcase_view.replace_external_links(Some(owner.source(self)));
        showcase_view.set_genres(self, owner.genres());
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
                if matches!(crate::widgets::media_drag::media_drag_source(&value), Some(crate::widgets::media_drag::MediaDragSource::PlaylistEntries(selection)) if selection.playlist == playlist && selection.writable && selection.count == 1) {
                    drop.reject();
                }
            });
            let drop_menus = Rc::downgrade(self.shell());
            drop.connect_drop(move |_, value, _, _| {
                let Some(menus) = drop_menus.upgrade() else {
                    return false;
                };
                let Some(source) = crate::widgets::media_drag::media_drag_source(value) else {
                    return false;
                };
                let PlaylistDetailOwner::Saved { summary, .. } = drop_owner.borrow().clone() else {
                    return false;
                };
                crate::menus::media_menus::add_drag_to_playlist(
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
        let menu = Rc::new(move |target: &gtk::Widget, position| {
            let row = menu_owner.borrow().row();
            present_row_menu(
                target,
                menu_shell.shell(),
                row,
                Some(Rc::clone(&menu_play)),
                position,
            );
        });
        let open_cover = {
            let shell = Rc::clone(self);
            let owner = Rc::clone(&owner_state);
            Rc::new(move || {
                let artwork = owner.borrow().artwork();
                shell.present_full_artwork_group(artwork);
            })
        };
        let showcase = showcase_view.mount(
            self,
            owner.row(),
            ShowcaseMount {
                shape: ShowcaseShape::Detail,
                initial_width: width,
                cover: ShowcaseCover::Group(cover.clone(), open_cover),
                play_label: msgid("Play playlist"),
                play: Rc::clone(&play),
                menu,
                download: true,
            },
        );
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
                showcase.replace_summary(&next.summary());
                showcase.replace_external_links(Some(next.source(&shell)));
                showcase.set_genres(&shell, next.genres());
                showcase.show_row(next.row());
                cover.replace(&shell.artwork, &next.artwork());
                owner.replace(next);
            }) as Rc<dyn Fn(Option<library::Header>)>
        };
        let reads = reads(Rc::clone(&apply_owner));
        let resume = {
            let shell = Rc::downgrade(self);
            let reads = reads.clone();
            Rc::new(move || {
                let Some(shell) = shell.upgrade() else {
                    return;
                };
                let settings = shell.settings.current.borrow().library_list(key);
                apply_list_settings(&settings);
                reads.resume(&shell.settings.current.borrow());
            })
        };
        MountedRoute::new(wrapper.upcast(), resume)
            .with_search(search)
            .with_layout_cycle(layout_cycle)
            .with_item_navigation(item_navigation)
            .with_initial_demand({
                let rows = rows.clone();
                Rc::new(move || rows.resume_initial_demand())
            })
            .with_catalog_refresh(Rc::new(move || reads.refresh()))
            .with_rows(&rows)
            .with_header(move || Some(owner_state.borrow().header()), apply_owner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

use crate::menus::media_menus::CollectionPlay;
