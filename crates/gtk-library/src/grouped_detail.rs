use std::rc::Rc;

use adw::prelude::*;
use artwork::ArtworkBinding;
use localization::msgid;

use crate::CatalogUi;
use crate::LibraryListKey;

use super::collections::library_route_inset;
use super::detail_showcase::{
    CollectionDetailShowcase, DetailShowcaseView, collection_detail_showcase,
    detail_playback_controls,
};
use super::playlist_detail::playlist_cover_size;
use super::route_shell::LibraryToolbarProjection;
use super::routes::{SearchableTrackOptions, TrackListProjection};
use crate::route_layout::{
    PRIMARY_ROUTE_HORIZONTAL_INSET, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
};
use gtk_media_menus::media_menus::CollectionPlay;

pub struct GroupedDetailData {
    pub key: LibraryListKey,
    pub kind: &'static str,
    pub genre_kind: bool,
    pub kind_controls: Vec<gtk::Widget>,
    pub title: String,
    pub artwork: Vec<ArtworkBinding>,
    pub seed: u32,
    pub summary_items: Vec<(&'static str, String)>,
    pub context_menu: Option<Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)>>,
    pub tracks: library::TrackRoutePage,
    pub table_context: &'static str,
    pub playback_context: String,
    pub play_label: &'static str,
}

#[derive(Clone)]
pub struct GroupedDetailView {
    root: gtk::Widget,
    tracks: TrackListProjection,
    toolbar: LibraryToolbarProjection,
}

impl GroupedDetailView {
    pub fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    pub fn tracks(&self) -> &TrackListProjection {
        &self.tracks
    }

    pub fn item_navigation(&self) -> gtk_widgets::mounted_route::MountedRouteItemNavigation {
        self.tracks.item_navigation()
    }

    pub fn search(&self) -> gtk::SearchEntry {
        self.tracks.search()
    }

    pub fn layout_cycle(&self) -> gtk_widgets::mounted_route::MountedRouteCommand {
        self.toolbar.layout_cycle()
    }
}

impl CatalogUi {
    pub fn grouped_detail_view(self: &Rc<Self>, data: GroupedDetailData) -> GroupedDetailView {
        let GroupedDetailData {
            key,
            kind,
            genre_kind,
            kind_controls,
            title,
            artwork,
            seed,
            summary_items,
            context_menu,
            tracks,
            table_context,
            playback_context,
            play_label,
        } = data;
        let content_width = crate::route_layout::detail_route_inner_width_for_viewport(
            (self.route_width)(),
            PRIMARY_ROUTE_MARGIN_START,
        );
        let cover_size = playlist_cover_size(content_width);
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 18);
        wrapper.add_css_class("route-content");
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);

        let cover = self.artwork.cover_group_projection_for_artwork(
            &artwork,
            cover_size,
            playlist_cover_size(i32::MAX),
        );
        let showcase_view =
            DetailShowcaseView::new("playlist-detail-showcase", seed, kind, genre_kind, &title);
        for control in kind_controls {
            showcase_view.append_kind_control(&control);
        }
        showcase_view.replace_summary(&summary_items);
        let track_projection = self.searchable_track_collection(
            tracks,
            key,
            SearchableTrackOptions {
                context_id: playback_context.clone(),
                content_inset: PRIMARY_ROUTE_HORIZONTAL_INSET,
                fixed_layout: None,
                search: None,
            },
        );
        let controller = self.queue.clone();
        let play_context = playback_context;
        let play_tracks = track_projection.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            play_tracks.play_source(
                controller.clone(),
                placement,
                play_context.clone(),
                shuffled,
            );
        });
        let actions = showcase_view.actions();
        actions.set_halign(gtk::Align::Start);
        let cover_controls = detail_playback_controls(&actions, play_label, None, Rc::clone(&play));
        let context_menu = context_menu.map(|present| {
            let play = Rc::clone(&play);
            Rc::new(move |target: &gtk::Widget, position| {
                present(target, position, Rc::clone(&play));
            }) as gtk_widgets::interactions::ContextMenuOpen
        });
        let showcase = collection_detail_showcase(CollectionDetailShowcase {
            view: showcase_view,
            initial_width: content_width,
            compact_spacing: 22,
            wide_spacing: 22,
            cover: cover.clone(),
            cover_controls,
            context_menu,
            open_cover: {
                let present = Rc::clone(&self.present_full_artwork);
                Rc::new(move || present(artwork.clone()))
            },
        });
        wrapper.append(&library_route_inset(showcase));

        let track_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        track_section.set_widget_name(table_context);
        track_section.set_hexpand(true);
        track_section.set_halign(gtk::Align::Fill);
        track_section.set_vexpand(true);
        let toolbar = self.library_toolbar_projection(key, track_projection.search());
        track_section.append(&library_route_inset(toolbar.widget()));
        track_section.append(&track_projection.scrolling_widget());

        let track_stack = gtk::Stack::new();
        track_stack.set_hexpand(true);
        track_stack.set_vexpand(true);
        track_stack.add_named(
            &library_route_inset(crate::route_layout::placeholder_view(
                "Tracks",
                msgid("No tracks here yet"),
            )),
            Some("empty"),
        );
        track_stack.add_named(&track_section, Some("tracks"));
        track_stack.set_visible_child_name(if track_projection.source_is_empty() {
            "empty"
        } else {
            "tracks"
        });
        wrapper.append(&track_stack);

        GroupedDetailView {
            root: wrapper.upcast(),
            tracks: track_projection,
            toolbar,
        }
    }
}
