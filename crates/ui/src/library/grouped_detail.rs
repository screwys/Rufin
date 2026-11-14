use std::{cell::RefCell, rc::Rc};

use adw::prelude::*;
use localization::msgid;
use rufin_core::artwork::ArtworkBinding;

use crate::library::CatalogUi;
use crate::library::LibraryListKey;
use crate::widgets::view_model::Rows;
use library::Row;

use super::collections::library_route_inset;
use super::detail_showcase::{DetailShowcaseView, ShowcaseCover, ShowcaseMount, ShowcaseShape};
use super::route_shell::LibraryToolbarProjection;
use super::routes::{SearchableTrackOptions, TrackListProjection};
use crate::library::route_layout::{
    PRIMARY_ROUTE_HORIZONTAL_INSET, PRIMARY_ROUTE_MARGIN_START, ROUTE_TOP_MARGIN,
    detail_showcase_cover_size,
};
use crate::menus::media_menus::CollectionPlay;

pub struct GroupedDetailData {
    pub key: LibraryListKey,
    /// The row the header's buttons act on.
    pub row: Row,
    pub title: String,
    pub artwork: Vec<ArtworkBinding>,
    pub summary_items: Vec<(&'static str, String)>,
    pub context_menu: Rc<dyn Fn(&gtk::Widget, Option<(f64, f64)>, CollectionPlay)>,
    pub rows: Rows<Row>,
    pub table_context: &'static str,
    pub play_label: &'static str,
}

#[derive(Clone)]
pub struct GroupedDetailView {
    root: gtk::Widget,
    tracks: TrackListProjection,
    toolbar: LibraryToolbarProjection,
    showcase: DetailShowcaseView,
    cover: crate::widgets::artwork::CoverGroupProjection,
    artwork: Rc<RefCell<Vec<ArtworkBinding>>>,
}

impl GroupedDetailView {
    pub fn widget(&self) -> gtk::Widget {
        self.root.clone()
    }

    pub fn tracks(&self) -> &TrackListProjection {
        &self.tracks
    }

    pub fn item_navigation(&self) -> crate::widgets::mounted_route::MountedRouteItemNavigation {
        self.tracks.item_navigation()
    }

    pub fn search(&self) -> gtk::SearchEntry {
        self.tracks.search()
    }

    pub fn layout_cycle(&self) -> crate::widgets::mounted_route::MountedRouteCommand {
        self.toolbar.layout_cycle()
    }

    /// Shows a header read again: its title, artwork, counts and the row its buttons act on.
    pub fn show(
        &self,
        shell: &CatalogUi,
        row: Row,
        title: &str,
        artwork: Vec<ArtworkBinding>,
        summary_items: &[(&str, String)],
    ) {
        self.showcase.set_title(title);
        self.showcase.replace_summary(summary_items);
        self.showcase.show_row(row);
        self.cover.replace(&shell.artwork, &artwork);
        self.artwork.replace(artwork);
    }
}

impl CatalogUi {
    pub fn grouped_detail_view(self: &Rc<Self>, data: GroupedDetailData) -> GroupedDetailView {
        let GroupedDetailData {
            key,
            row,
            title,
            artwork,
            summary_items,
            context_menu,
            rows,
            table_context,
            play_label,
        } = data;
        let content_width = crate::library::route_layout::detail_route_inner_width_for_viewport(
            self.route_width(),
            PRIMARY_ROUTE_MARGIN_START,
        );
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 18);
        wrapper.add_css_class("route-content");
        wrapper.set_margin_top(ROUTE_TOP_MARGIN);
        wrapper.set_hexpand(true);
        wrapper.set_halign(gtk::Align::Fill);
        wrapper.set_width_request(1);
        wrapper.set_vexpand(true);

        let cover = self.artwork.cover_group_projection_for_artwork(
            &artwork,
            ShowcaseShape::Detail.cover_size(content_width),
            detail_showcase_cover_size(i32::MAX),
            true,
        );
        let showcase_view = DetailShowcaseView::new(&title);
        showcase_view.replace_summary(&summary_items);
        let track_projection = self.track_projection(
            rows.clone(),
            key,
            SearchableTrackOptions {
                content_inset: PRIMARY_ROUTE_HORIZONTAL_INSET,
                fixed_layout: None,
                search: None,
            },
        );
        let playback = self.products.playback.clone();
        let play: CollectionPlay = Rc::new(move |placement, shuffled| {
            rows.play_all(&playback, placement, shuffled);
        });
        let menu = {
            let play = Rc::clone(&play);
            Rc::new(move |target: &gtk::Widget, position| {
                context_menu(target, position, Rc::clone(&play));
            }) as crate::widgets::interactions::ContextMenuOpen
        };
        let artwork = Rc::new(RefCell::new(artwork));
        let open_cover = {
            let shell = Rc::clone(self);
            let artwork = Rc::clone(&artwork);
            Rc::new(move || {
                let bindings = artwork.borrow().clone();
                shell.present_full_artwork_group(bindings);
            })
        };
        let showcase = showcase_view.mount(
            self,
            row,
            ShowcaseMount {
                shape: ShowcaseShape::Detail,
                initial_width: content_width,
                cover: ShowcaseCover::Group(cover.clone(), open_cover),
                play_label,
                play,
                menu,
                download: true,
            },
        );
        wrapper.append(&library_route_inset(showcase));

        let track_section = gtk::Box::new(gtk::Orientation::Vertical, 10);
        track_section.set_widget_name(table_context);
        track_section.set_hexpand(true);
        track_section.set_halign(gtk::Align::Fill);
        track_section.set_vexpand(true);
        let toolbar = track_projection.toolbar(self);
        track_section.append(&library_route_inset(toolbar.widget()));
        track_section.append(&track_projection.scrolling_widget());

        let track_stack = gtk::Stack::new();
        track_stack.set_hexpand(true);
        track_stack.set_vexpand(true);
        track_stack.add_named(
            &library_route_inset(crate::library::route_layout::placeholder_view(
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
            showcase: showcase_view,
            cover,
            artwork,
        }
    }
}
