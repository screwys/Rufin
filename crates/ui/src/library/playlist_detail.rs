use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use library::{PlaylistKey, PlaylistRow, QuickMixRow, SmartPlaylistRow};
use localization::{msgid, track_count_text};
use rufin_core::artwork::ArtworkBinding;

use crate::library::CatalogUi;
use crate::library::routes::PageReads;
use crate::library::{LibraryListKey, LibraryListSettings};
use crate::widgets::mounted_route::MountedRoute;
use crate::widgets::view_model::Rows;
use rufin_core::route::Route;
use rufin_core::settings::presentation::{format_duration_units, home_block_title};
use rufin_core::view::Opened;

use super::collections::library_route_inset;
use super::detail_showcase::{
    DetailShowcaseView, ShowcaseCover, ShowcaseMount, ShowcaseShape, ShowcaseSubject,
    quick_mix_menu, source_link,
};
use crate::library::route_layout::{
    PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN, detail_showcase_cover_size,
};
use crate::menus::media_menus::{present_quick_mix_menu, present_row_menu};
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
    /// A Quick mix entry, shown as a smart playlist: the tracks its next press plays.
    QuickMix {
        summary: QuickMixRow,
    },
}

impl PlaylistDetailOwner {
    /// The header it shows.
    fn header(&self) -> library::Header {
        match self {
            Self::Saved { summary, .. } => library::Header::Playlist(Box::new(summary.clone())),
            Self::Smart { summary, .. } => library::Header::Smart(Box::new(summary.clone())),
            Self::QuickMix { summary } => library::Header::QuickMix(Box::new(summary.clone())),
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
            Self::Smart { .. } | Self::QuickMix { .. } => None,
        };
        catalog.source_label(source_id)
    }

    fn key(&self) -> LibraryListKey {
        match self {
            Self::Saved { .. } => LibraryListKey::PlaylistTracks,
            Self::Smart { .. } | Self::QuickMix { .. } => LibraryListKey::SmartPlaylistTracks,
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Saved { summary, .. } => &summary.name,
            Self::Smart { summary, .. } => &summary.name,
            Self::QuickMix { summary } => &summary.name,
        }
    }

    fn track_count(&self) -> i64 {
        match self {
            Self::Saved { summary, .. } => summary.track_count,
            Self::Smart { summary, .. } => summary.track_count,
            Self::QuickMix { summary } => summary.track_count,
        }
    }

    fn duration_millis(&self) -> i64 {
        match self {
            Self::Saved { summary, .. } => summary.duration_millis,
            Self::Smart { summary, .. } => summary.duration_millis,
            Self::QuickMix { summary } => summary.duration_millis,
        }
    }

    fn artwork(&self) -> Vec<ArtworkBinding> {
        match self {
            Self::Saved { summary, .. } => playlist_artwork(summary),
            Self::Smart { summary } => smart_artwork(&summary.artwork_binding),
            Self::QuickMix { summary } => smart_artwork(&summary.artwork_binding),
        }
    }

    fn saved_key(&self) -> Option<PlaylistKey> {
        match self {
            Self::Saved { key, .. } => Some(*key),
            Self::Smart { .. } | Self::QuickMix { .. } => None,
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
            (Self::QuickMix { .. }, library::Header::QuickMix(summary)) => {
                Self::QuickMix { summary: *summary }
            }
            _ => return None,
        })
    }

    /// What its menu and header buttons act on; a smart playlist and a Quick mix start their
    /// radio from the first of `rows`, the tracks the page lists.
    fn subject(&self, rows: &Rows<library::Row>) -> ShowcaseSubject {
        match self {
            Self::Saved { summary, .. } => ShowcaseSubject::Row(library::Row::Playlist(
                PlaylistListRow::Playlist(summary.clone()),
            )),
            Self::Smart { summary, .. } => ShowcaseSubject::Smart(
                library::Row::Playlist(PlaylistListRow::SmartPlaylist(summary.clone())),
                rows.clone(),
            ),
            Self::QuickMix { summary } => ShowcaseSubject::QuickMix {
                name: summary.name.clone(),
                rows: rows.clone(),
                downloaded: summary.downloaded(),
            },
        }
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
            Self::Smart { .. } | Self::QuickMix { .. } => Vec::new(),
        }
    }
}

impl CatalogUi {
    /// A smart playlist's page, and a Quick mix entry's, which shows as one.
    pub fn smart_playlist_detail_route(self: &Rc<Self>, mut read: Opened) -> MountedRoute {
        let owner = match (read.view.is_missing(), read.header.take()) {
            (false, Some(library::Header::Smart(summary))) => {
                PlaylistDetailOwner::Smart { summary: *summary }
            }
            (false, Some(library::Header::QuickMix(summary))) => {
                PlaylistDetailOwner::QuickMix { summary: *summary }
            }
            _ => {
                let title = match read.view.route() {
                    Route::QuickMix(_) => home_block_title(library::HomeBlockKind::QuickMix),
                    _ => msgid("Smart Playlist"),
                };
                return MountedRoute::static_widget(
                    crate::library::route_layout::placeholder_view(
                        title,
                        msgid("This isn't available"),
                    ),
                );
            }
        };
        let list_key = owner.key();
        let rows = Rows::over_view(self.products.runtime.clone(), &mut read);
        let play_rows = rows.clone();
        let play_queue = self.products.playback.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            play_rows.play_all(&play_queue, placement, shuffled);
        });
        let (tracks_widget, tracks, toolbar) =
            self.scrolling_track_projection(rows.clone(), list_key, "smart-playlist-detail");
        let scroll_parts = tracks.scroll_parts();
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
            scroll_parts,
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
        let scroll_parts = entries.scroll_parts();
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
            scroll_parts,
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
        scroll_parts: (gtk::ScrolledWindow, gtk::Widget),
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
        let menu_rows = rows.clone();
        let menu_play = Rc::clone(&play);
        let menu = Rc::new(move |target: &gtk::Widget, position| {
            let shell = menu_shell.shell();
            // A Quick mix's menu acts on everything its page lists.
            let subject = menu_owner.borrow().subject(&menu_rows);
            match subject {
                ShowcaseSubject::Row(row) | ShowcaseSubject::Smart(row, _) => {
                    present_row_menu(target, shell, row, Some(Rc::clone(&menu_play)), position)
                }
                ShowcaseSubject::QuickMix {
                    name,
                    rows,
                    downloaded,
                } => {
                    if let Some(selection) = rows.whole_selection() {
                        let menu = quick_mix_menu(&menu_shell, &rows, downloaded, false);
                        let media =
                            crate::widgets::media_drag::MediaDragSource::selection(selection);
                        let play = Rc::clone(&menu_play);
                        present_quick_mix_menu(target, shell, &menu, (name, media), play, position);
                    }
                }
            }
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
            owner.subject(&rows),
            ShowcaseMount {
                shape: ShowcaseShape::Detail,
                initial_width: width,
                cover: ShowcaseCover::Group(cover.clone(), open_cover),
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
            let rows = rows.clone();
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
                showcase.show(next.subject(&rows));
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
        MountedRoute::new(showcase_view.scrolling_page(&wrapper, scroll_parts), resume)
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

/// A smart playlist's mosaic, which a Quick mix's header and Home tile show too.
pub(super) fn smart_artwork(binding: &Option<Vec<u8>>) -> Vec<ArtworkBinding> {
    binding
        .iter()
        .map(|binding| ArtworkBinding::opaque(binding))
        .collect()
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
