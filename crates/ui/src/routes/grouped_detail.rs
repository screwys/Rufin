use std::rc::Rc;

use adw::prelude::*;
use artwork::ArtworkBinding;
use localization::msgid;

use crate::LibraryListKey;
use crate::shell::Shell;

use super::collections::CollectionPlay;
use super::collections::library_route_inset;
use super::detail_showcase::{
    CollectionDetailShowcase, DetailSummaryProjection, collection_detail_showcase,
    detail_action_row, detail_playback_controls,
};
use super::playlist_detail::playlist_cover_size;
use super::route_layout::{
    PRIMARY_ROUTE_HORIZONTAL_INSET, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
    detail_route_inner_width,
};
use super::routes::{SearchableTrackOptions, TrackListProjection};

pub(crate) struct GroupedDetailData {
    pub(super) key: LibraryListKey,
    pub(super) kind_row: Option<gtk::Widget>,
    pub(super) title: String,
    pub(super) artwork: Vec<ArtworkBinding>,
    pub(super) seed: u32,
    pub(super) summary_items: Vec<(&'static str, String)>,
    pub(super) context_menu: Option<Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)>>,
    pub(super) selected: crate::runtime::SelectedLibrary,
    pub(super) tracks: Vec<library::TrackKey>,
    pub(super) first_rows: Vec<library::TrackRow>,
    pub(super) play_order: Option<Vec<library::TrackKey>>,
    pub(super) table_context: &'static str,
    pub(super) playback_context: String,
    pub(super) play_label: &'static str,
}

#[derive(Clone)]
pub(crate) struct GroupedDetailView {
    root: gtk::Widget,
    tracks: TrackListProjection,
}

impl GroupedDetailView {
    pub(crate) fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    pub(crate) fn tracks(&self) -> &TrackListProjection {
        &self.tracks
    }

    pub(crate) fn item_navigation(&self) -> crate::shell::route::MountedRouteItemNavigation {
        self.tracks.item_navigation()
    }
}

impl Shell {
    pub(crate) fn grouped_detail_view(
        self: &Rc<Self>,
        data: GroupedDetailData,
    ) -> GroupedDetailView {
        let GroupedDetailData {
            key,
            kind_row,
            title,
            artwork,
            seed,
            summary_items,
            context_menu,
            selected,
            tracks,
            first_rows,
            play_order,
            table_context,
            playback_context,
            play_label,
        } = data;
        let content_width = detail_route_inner_width(self, PRIMARY_ROUTE_MARGIN_START);
        let cover_size = playlist_cover_size(content_width);
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 18);
        wrapper.add_css_class("route-content");
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);

        let cover = self.cover_group_projection_for_artwork(
            &artwork,
            cover_size,
            playlist_cover_size(i32::MAX),
        );
        let track_projection = self.searchable_track_collection(
            &selected,
            tracks,
            first_rows,
            key,
            SearchableTrackOptions {
                on_visible_count_changed: None,
                context_id: playback_context.clone(),
                content_inset: PRIMARY_ROUTE_HORIZONTAL_INSET,
                fixed_layout: None,
                search: None,
            },
        );
        let controller = self.products.playback.queue.clone();
        let play_context = playback_context;
        let play: CollectionPlay = if let Some(order) = play_order {
            let database = std::sync::Arc::clone(&selected.database);
            let source = selected.source_key;
            let epoch = selected.source_session_epoch;
            let runtime = selected.runtime.clone();
            let order: std::sync::Arc<[library::TrackKey]> = order.into();
            Rc::new(move |placement, shuffled_start| {
                let Some(anchor_key) = order.first().copied() else {
                    return;
                };
                let database = std::sync::Arc::clone(&database);
                let controller = controller.clone();
                let order = std::sync::Arc::clone(&order);
                let context = play_context.clone();
                runtime.spawn(async move {
                    let cancellation = library::ReadCancellation::new();
                    let Some(anchor) = database
                        .track_rows(source, &[anchor_key], &cancellation)
                        .await
                        .ok()
                        .and_then(|mut rows| rows.pop())
                    else {
                        return;
                    };
                    if let Some(request) = playback::LoadedPlayRequest::context(
                        source,
                        epoch,
                        order,
                        playback::PlaybackMedia::from(anchor),
                        0,
                        placement,
                        context,
                        shuffled_start,
                    ) {
                        controller.play_loaded(request);
                    }
                });
            })
        } else {
            let play_tracks = track_projection.clone();
            Rc::new(move |placement, shuffled_start| {
                play_tracks.play_source(
                    controller.clone(),
                    placement,
                    play_context.clone(),
                    shuffled_start,
                );
            })
        };
        let actions = detail_action_row();
        actions.set_halign(gtk::Align::Start);
        let cover_controls =
            detail_playback_controls(&actions, play_label, None, true, Rc::clone(&play));
        let context_menu = context_menu.map(|present| {
            let play = Rc::clone(&play);
            Rc::new(move |target: &gtk::Widget, position| {
                present(target, position, Rc::clone(&play));
            }) as crate::interactions::ContextMenuOpen
        });
        let title_label = gtk::Label::new(Some(&title));
        title_label.add_css_class("detail-title");
        title_label.set_xalign(0.0);
        title_label.set_wrap(true);
        title_label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        let mut metadata = Vec::new();
        if let Some(kind_row) = kind_row {
            metadata.push(kind_row);
        }
        metadata.push(title_label.clone().upcast());
        let summary = DetailSummaryProjection::new(&summary_items);
        metadata.push(summary.widget());
        metadata.push(actions.upcast());
        let showcase = collection_detail_showcase(
            self,
            CollectionDetailShowcase {
                seed,
                initial_width: content_width,
                compact_spacing: 22,
                wide_spacing: 22,
                cover: cover.clone(),
                cover_controls,
                context_menu,
                metadata,
            },
        );
        wrapper.append(&library_route_inset(showcase));

        let track_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        track_section.set_widget_name(table_context);
        track_section.set_hexpand(true);
        track_section.set_halign(gtk::Align::Fill);
        track_section.set_vexpand(true);
        let toolbar = self.library_toolbar_projection(key, track_projection.search());
        track_section.append(&library_route_inset(toolbar.widget()));
        self.set_route_search(Some(track_projection.search()));
        track_section.append(&track_projection.scrolling_widget());

        let track_stack = gtk::Stack::new();
        track_stack.set_hexpand(true);
        track_stack.set_vexpand(true);
        track_stack.add_named(
            &library_route_inset(self.placeholder_view("Tracks", msgid("No tracks here yet"))),
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
        }
    }
}
