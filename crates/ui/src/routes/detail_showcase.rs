use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use ::library::{Album, Artist};
use adw::prelude::*;
use artwork::ArtworkBinding;
use tracing::warn;

use crate::interactions::{ContextMenuOpen, add_widget_click, install_context_menu_openers};
use crate::layout::width_allocation_owner;
use crate::localization::bind_label_text_with;
use crate::shell::Shell;
use crate::shell::actions::{ActionButtonVariant, configure_action_button, icon_button};
use crate::shell::actions::{DELETE_ICON, PLAY_LATER_ICON, PLAY_NEXT_ICON};
use crate::shell::cover::presentation::add_album_seed_gradient_class;
use crate::shell::cover::{
    ArtworkTile, CoverGroupProjection, LARGE_COVER_SIZE, cover_fetch_size_for_display,
};
use localization::{msgid, tr};

use super::cards::{
    CoverHoverControls, cover_hover_controls, cover_play_hover_controls,
    elastic_cover_context_point,
};
use super::collections::CollectionPlay;
use super::detail_links::{DetailEntityKind, DetailExternalLink, server_entity_link};
use super::route_layout::{detail_showcase_cover_only, detail_showcase_cover_size};

const DETAIL_HEADER_SPACING: i32 = 18;
const RADIO_ICON: &str = "rufin-audio-radio-symbolic";

pub(crate) struct MediaDetailShowcase {
    pub(crate) route_class: &'static str,
    pub(crate) seed: u32,
    pub(crate) initial_width: i32,
    pub(crate) cover: DetailCoverProjection,
    pub(crate) cover_controls: CoverHoverControls,
    pub(crate) context_menu: Option<ContextMenuOpen>,
    pub(crate) external_links: DetailExternalLinksProjection,
    pub(crate) text_stack: gtk::Widget,
    pub(crate) actions: gtk::Widget,
}

#[derive(Clone)]
pub(crate) struct DetailExternalLinksProjection {
    root: gtk::Box,
    cover_only: Rc<Cell<bool>>,
}

impl DetailExternalLinksProjection {
    pub(crate) fn new(class: Option<&'static str>, links: Option<gtk::Widget>) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        if let Some(class) = class {
            root.add_css_class(class);
        }
        root.set_halign(gtk::Align::Center);
        let projection = Self {
            root,
            cover_only: Rc::new(Cell::new(false)),
        };
        projection.replace(links);
        projection
    }

    fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub(crate) fn replace(&self, links: Option<gtk::Widget>) {
        while let Some(child) = self.root.first_child() {
            self.root.remove(&child);
        }
        if let Some(links) = links {
            links.set_halign(gtk::Align::Center);
            self.root.append(&links);
        }
        self.apply_visibility();
    }

    fn set_cover_only(&self, cover_only: bool) {
        self.cover_only.set(cover_only);
        self.apply_visibility();
    }

    fn apply_visibility(&self) {
        let has_links = self.root.first_child().is_some();
        let cover_only = self.cover_only.get();
        let visible = detail_external_links_visible(has_links, cover_only);
        self.root.set_visible(visible);
    }
}

fn detail_external_links_visible(has_links: bool, cover_only: bool) -> bool {
    has_links && !cover_only
}

pub(crate) struct CollectionDetailShowcase {
    pub(crate) seed: u32,
    pub(crate) initial_width: i32,
    pub(crate) compact_spacing: i32,
    pub(crate) wide_spacing: i32,
    pub(crate) cover: CoverGroupProjection,
    pub(crate) cover_controls: CoverHoverControls,
    pub(crate) context_menu: Option<ContextMenuOpen>,
    pub(crate) metadata: Vec<gtk::Widget>,
}

pub(crate) struct PlaylistDetailShowcase {
    pub(crate) seed: u32,
    pub(crate) initial_width: i32,
    pub(crate) cover: CoverGroupProjection,
    pub(crate) cover_controls: CoverHoverControls,
    pub(crate) context_menu: Option<ContextMenuOpen>,
    pub(crate) kind_row: gtk::Widget,
    pub(crate) title: gtk::Widget,
    pub(crate) summary: gtk::Widget,
    pub(crate) actions: gtk::Widget,
}

pub(crate) fn media_detail_showcase(shell: &Rc<Shell>, config: MediaDetailShowcase) -> gtk::Widget {
    let header = gtk::Box::new(gtk::Orientation::Vertical, 12);
    header.add_css_class("detail-showcase");
    header.add_css_class(config.route_class);
    header.add_css_class("detail-showcase-horizontal");
    add_album_seed_gradient_class(&header, config.seed);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, DETAIL_HEADER_SPACING);
    body.set_hexpand(true);
    body.set_halign(gtk::Align::Fill);
    body.set_width_request(1);

    let cover_column = gtk::Box::new(gtk::Orientation::Vertical, 8);
    cover_column.set_halign(gtk::Align::Start);
    cover_column.append(&detail_cover_overlay(
        config.cover.button().upcast_ref(),
        config.cover_controls,
        config.context_menu,
    ));
    cover_column.append(&config.external_links.widget());
    body.append(&cover_column);

    let metadata = gtk::Box::new(gtk::Orientation::Vertical, 10);
    metadata.set_hexpand(true);
    metadata.set_valign(gtk::Align::Start);
    metadata.set_halign(gtk::Align::Fill);
    metadata.set_width_request(1);
    metadata.append(&config.text_stack);
    metadata.append(&config.actions);
    body.append(&metadata);

    header.append(&body);
    let presentation = MediaShowcasePresentation {
        viewport_width: Rc::new(Cell::new(0)),
        cover_width: Rc::new(Cell::new(0)),
        header: header.clone(),
        cover_column,
        cover: config.cover,
        external_links: config.external_links,
        metadata,
    };
    presentation.apply_viewport_width(config.initial_width);
    presentation.resize_cover(config.initial_width);
    let frame = detail_showcase_frame_with_back(shell, header.upcast());
    width_allocation_owner(&frame, move |width| {
        presentation.apply_viewport_width(width);
        presentation.resize_cover(width);
    })
    .upcast()
}

#[derive(Clone)]
struct MediaShowcasePresentation {
    viewport_width: Rc<Cell<i32>>,
    cover_width: Rc<Cell<i32>>,
    header: gtk::Box,
    cover_column: gtk::Box,
    cover: DetailCoverProjection,
    external_links: DetailExternalLinksProjection,
    metadata: gtk::Box,
}

impl MediaShowcasePresentation {
    fn apply_viewport_width(&self, width: i32) {
        if width <= 1 || self.viewport_width.replace(width) == width {
            return;
        }
        let cover_only = detail_showcase_cover_only(width);
        update_tiny_detail_showcase(&self.header, width);
        self.external_links.set_cover_only(cover_only);
        self.metadata.set_visible(!cover_only);
    }

    fn resize_cover(&self, width: i32) {
        if width <= 1 || self.cover_width.replace(width) == width {
            return;
        }
        let cover_size = super::route_layout::detail_showcase_cover_size(width);
        self.cover_column.set_width_request(cover_size);
        self.cover.resize(cover_size);
    }
}

pub(crate) fn collection_detail_showcase(
    shell: &Rc<Shell>,
    config: CollectionDetailShowcase,
) -> gtk::Widget {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, config.wide_spacing);
    header.add_css_class("playlist-detail-showcase");
    add_album_seed_gradient_class(&header, config.seed);
    header.set_hexpand(true);
    header.set_halign(gtk::Align::Fill);
    header.set_width_request(1);
    header.append(&detail_cover_overlay(
        &config.cover.widget(),
        config.cover_controls,
        config.context_menu,
    ));

    let metadata = gtk::Box::new(gtk::Orientation::Vertical, 10);
    metadata.set_valign(gtk::Align::Center);
    metadata.set_hexpand(true);
    metadata.set_halign(gtk::Align::Fill);
    metadata.set_width_request(1);
    for child in config.metadata {
        metadata.append(&child);
    }
    header.append(&metadata);

    let presentation = CollectionShowcasePresentation {
        viewport_width: Rc::new(Cell::new(0)),
        cover_width: Rc::new(Cell::new(0)),
        header: header.clone(),
        cover: config.cover,
        metadata,
        compact_spacing: config.compact_spacing,
        wide_spacing: config.wide_spacing,
    };
    presentation.apply_viewport_width(config.initial_width);
    presentation.resize_cover(config.initial_width);
    let frame = detail_showcase_frame_with_back(shell, header.upcast());
    width_allocation_owner(&frame, move |width| {
        presentation.apply_viewport_width(width);
        presentation.resize_cover(width);
    })
    .upcast()
}

#[derive(Clone)]
struct CollectionShowcasePresentation {
    viewport_width: Rc<Cell<i32>>,
    cover_width: Rc<Cell<i32>>,
    header: gtk::Box,
    cover: CoverGroupProjection,
    metadata: gtk::Box,
    compact_spacing: i32,
    wide_spacing: i32,
}

impl CollectionShowcasePresentation {
    fn apply_viewport_width(&self, width: i32) {
        if width <= 1 || self.viewport_width.replace(width) == width {
            return;
        }
        let cover_only = detail_showcase_cover_only(width);
        update_tiny_detail_showcase(&self.header, width);
        self.header.set_spacing(
            if super::playlist_detail::playlist_detail_compact_for_width(width) {
                self.compact_spacing
            } else {
                self.wide_spacing
            },
        );
        self.metadata.set_visible(!cover_only);
    }

    fn resize_cover(&self, width: i32) {
        if width <= 1 || self.cover_width.replace(width) == width {
            return;
        }
        let cover_size = super::playlist_detail::playlist_cover_size(width);
        self.cover.resize(cover_size);
    }
}

pub(crate) fn playlist_detail_showcase(
    shell: &Rc<Shell>,
    config: PlaylistDetailShowcase,
) -> gtk::Widget {
    collection_detail_showcase(
        shell,
        CollectionDetailShowcase {
            seed: config.seed,
            initial_width: config.initial_width,
            compact_spacing: 20,
            wide_spacing: 28,
            cover: config.cover,
            cover_controls: config.cover_controls,
            context_menu: config.context_menu,
            metadata: vec![
                config.kind_row,
                config.title,
                config.summary,
                config.actions,
            ],
        },
    )
}

#[derive(Clone)]
pub(crate) struct DetailSummaryProjection {
    root: gtk::Box,
    items: Rc<Vec<(gtk::Box, gtk::Image, gtk::Label)>>,
}

impl DetailSummaryProjection {
    pub(crate) fn new(items: &[(&str, String)]) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        root.add_css_class("detail-summary-row");
        root.set_halign(gtk::Align::Start);
        let slots = Rc::new(
            (0..3)
                .map(|_| {
                    let item = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                    let icon = gtk::Image::new();
                    icon.add_css_class("muted");
                    icon.set_pixel_size(14);
                    item.append(&icon);
                    let label = gtk::Label::new(None);
                    label.add_css_class("muted");
                    label.set_xalign(0.0);
                    item.append(&label);
                    root.append(&item);
                    (item, icon, label)
                })
                .collect::<Vec<_>>(),
        );
        let projection = Self { root, items: slots };
        projection.replace(items);
        projection
    }

    pub(crate) fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub(crate) fn replace(&self, values: &[(&str, String)]) {
        for (index, (item, icon, label)) in self.items.iter().enumerate() {
            if let Some((icon_name, text)) = values
                .get(index)
                .filter(|(_, text)| summary_value_is_visible(text))
            {
                icon.set_icon_name(Some(icon_name));
                label.set_text(text);
                item.set_visible(true);
            } else {
                item.set_visible(false);
            }
        }
    }

    pub(crate) fn bind_text_with(&self, index: usize, text: impl Fn() -> String + 'static) {
        if let Some((_, _, label)) = self.items.get(index) {
            bind_label_text_with(label, text);
        }
    }
}

fn summary_value_is_visible(text: &str) -> bool {
    !text.is_empty()
}

pub(crate) fn detail_action_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = icon_button(icon_name, label);
    configure_action_button(&button, ActionButtonVariant::DetailAction, Some(icon_name));
    button
}

pub(crate) fn detail_primary_action_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = icon_button(icon_name, label);
    configure_action_button(&button, ActionButtonVariant::DetailPrimary, Some(icon_name));
    button
}

pub(crate) fn detail_radio_button() -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("detail-kind-radio-button");
    button.set_halign(gtk::Align::Start);
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(&tr("Play radio")));
    let icon = gtk::Image::from_icon_name(RADIO_ICON);
    icon.set_pixel_size(16);
    button.set_child(Some(&icon));
    button
}

pub(crate) fn detail_playback_controls(
    actions: &gtk::Box,
    play_label: &str,
    favorite_active: Option<bool>,
    show_queue_actions: bool,
    play: CollectionPlay,
) -> CoverHoverControls {
    let controls = favorite_active.map_or_else(
        || cover_play_hover_controls(0, play_label),
        |favorite| cover_hover_controls(0, play_label, favorite),
    );

    let primary = detail_primary_action_button(crate::shell::actions::PLAY_ICON, "Play");
    connect_collection_play(
        &primary,
        Rc::clone(&play),
        playback::QueuePlacement::Now,
        true,
    );
    actions.append(&primary);

    for (icon, label, placement, hover) in [
        (
            PLAY_NEXT_ICON,
            "Next",
            playback::QueuePlacement::Next,
            controls.play_next.clone(),
        ),
        (
            PLAY_LATER_ICON,
            "Play Later",
            playback::QueuePlacement::Last,
            controls.play_last.clone(),
        ),
    ] {
        if show_queue_actions {
            let button = detail_action_button(icon, label);
            connect_collection_play(&button, Rc::clone(&play), placement, false);
            actions.append(&button);
        }
        connect_collection_play(&hover, Rc::clone(&play), placement, false);
    }
    connect_collection_play(&controls.play, play, playback::QueuePlacement::Now, true);
    controls
}

fn connect_collection_play(
    button: &gtk::Button,
    play: CollectionPlay,
    placement: playback::QueuePlacement,
    shuffled_start: bool,
) {
    button.connect_clicked(move |_| play(placement, shuffled_start));
}

fn detail_cover_overlay(
    cover: &gtk::Widget,
    mut controls: CoverHoverControls,
    context_menu: Option<ContextMenuOpen>,
) -> gtk::Overlay {
    let overlay = gtk::Overlay::new();
    overlay.add_css_class("cover-frame");
    overlay.set_halign(gtk::Align::Start);
    overlay.set_valign(gtk::Align::Start);
    overlay.set_child(Some(cover));

    if let Some(open) = context_menu {
        let menu = controls.add_context_button();
        install_context_menu_openers(&overlay, Rc::clone(&open));
        let target = overlay.downgrade();
        menu.connect_clicked(move |_| {
            let Some(target) = target.upgrade() else {
                return;
            };
            open(target.upcast_ref(), elastic_cover_context_point(&target));
        });
    }
    controls.add_to_overlay(&overlay);
    controls.connect_hover(&overlay);
    overlay
}

pub(crate) fn detail_title_label(text: &str) -> gtk::Label {
    let title = gtk::Label::new(Some(text));
    title.add_css_class("detail-title");
    title.set_xalign(0.0);
    title.set_wrap(true);
    title.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    title.set_hexpand(true);
    title.set_halign(gtk::Align::Fill);
    title.set_width_request(1);
    title.set_width_chars(1);
    title.set_max_width_chars(32);
    title
}

pub(crate) fn fitted_detail_title_label(text: &str) -> gtk::Label {
    let title = detail_title_label(text);
    title.set_justify(gtk::Justification::Left);
    fit_detail_text(&title, text);
    title
}

pub(crate) fn detail_genre_pill_button(label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("album-detail-genre-pill");
    button.set_halign(gtk::Align::Start);
    button.set_valign(gtk::Align::Center);
    button.set_hexpand(false);
    button.set_tooltip_text(Some(label));
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_halign(gtk::Align::Start);
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    text.set_width_chars(1);
    text.set_max_width_chars(28);
    button.set_child(Some(&text));
    button
}

pub(crate) fn detail_delete_button(label: &str) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.set_valign(gtk::Align::Center);
    button.set_tooltip_text(Some(&tr(label)));
    button.set_child(Some(&gtk::Image::from_icon_name(DELETE_ICON)));
    configure_action_button(
        &button,
        ActionButtonVariant::DetailAction,
        Some(DELETE_ICON),
    );
    button
}

pub(crate) fn detail_action_row() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_css_class("detail-showcase-actions");
    row.set_halign(gtk::Align::Center);
    row
}

#[derive(Clone)]
pub(crate) struct DetailCoverProjection {
    button: gtk::Button,
    tile: ArtworkTile,
    size: Rc<Cell<i32>>,
    candidates: Rc<RefCell<ArtworkBinding>>,
    render_size: i32,
    fetch_size: u32,
}

impl DetailCoverProjection {
    pub(crate) fn button(&self) -> gtk::Button {
        self.button.clone()
    }

    pub(crate) fn resize(&self, size: i32) {
        let size = size.max(1);
        if self.size.replace(size) == size {
            return;
        }
        self.button.set_size_request(size, size);
        self.tile.set_square_size(size);
    }

    pub(crate) fn replace(&self, shell: &Rc<Shell>, binding: ArtworkBinding) {
        self.candidates.replace(binding.clone());
        shell.bind_artwork_tile(&self.tile, binding, self.render_size, self.fetch_size);
    }
}

pub(crate) fn detail_cover_projection(
    shell: &Rc<Shell>,
    candidates: ArtworkBinding,
    size: i32,
    cover_class: &str,
) -> DetailCoverProjection {
    let render_size = detail_cover_render_size();
    let fetch_size = LARGE_COVER_SIZE;
    let tile = ArtworkTile::new_sized(size, size);
    shell.bind_artwork_tile(&tile, candidates.clone(), render_size, fetch_size);
    let cover = tile.widget();
    cover.add_css_class("detail-showcase-cover");
    cover.add_css_class(cover_class);

    let button = gtk::Button::new();
    button.add_css_class("flat");
    button.add_css_class("detail-cover-button");
    button.set_halign(gtk::Align::Start);
    button.set_valign(gtk::Align::Start);
    button.set_cursor_from_name(Some("pointer"));
    button.set_child(Some(&cover));

    let candidates = Rc::new(RefCell::new(candidates));
    let open_candidates = Rc::clone(&candidates);
    let shell = Rc::clone(shell);
    button.connect_clicked(move |_| {
        let candidates = open_candidates.borrow().clone();
        shell.present_full_artwork(candidates);
    });
    DetailCoverProjection {
        button,
        tile,
        size: Rc::new(Cell::new(size)),
        candidates,
        render_size,
        fetch_size,
    }
}

fn detail_cover_render_size() -> i32 {
    detail_showcase_cover_size(i32::MAX)
}

impl Shell {
    fn present_full_artwork(self: &Rc<Self>, candidates: ArtworkBinding) {
        let size = full_artwork_size(self.chrome.window.width(), self.chrome.window.height());
        let fetch_size = cover_fetch_size_for_display(size);
        let tile = ArtworkTile::new_sized(size, size);
        let cover = tile.widget();
        self.bind_artwork_tile(&tile, candidates, size, fetch_size);
        cover.add_css_class("full-artwork-cover");
        cover.set_halign(gtk::Align::Center);
        cover.set_valign(gtk::Align::Center);

        let root = gtk::Overlay::new();
        root.add_css_class("full-artwork-window");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_child(Some(&cover));

        self.chrome.app_root_overlay.add_overlay(&root);
        self.chrome
            .app_root_overlay
            .set_measure_overlay(&root, false);

        let overlay = self.chrome.app_root_overlay.downgrade();
        let root_for_close = root.downgrade();
        let tile_for_close = tile.downgrade();
        let shell_for_close = Rc::downgrade(self);
        add_widget_click(root.upcast_ref(), move || {
            if let (Some(shell), Some(tile)) = (shell_for_close.upgrade(), tile_for_close.upgrade())
            {
                shell.clear_artwork_tile(&tile);
            }
            if let (Some(overlay), Some(root)) = (overlay.upgrade(), root_for_close.upgrade()) {
                overlay.remove_overlay(&root);
            }
        });
    }
}

fn full_artwork_size(width: i32, height: i32) -> i32 {
    (width.min(height) - 80).clamp(240, 720)
}

pub(crate) fn detail_showcase_frame(header: gtk::Widget) -> gtk::Widget {
    header.set_hexpand(true);
    header.set_halign(gtk::Align::Fill);
    header.set_width_request(1);
    header
}

pub(crate) fn detail_showcase_frame_with_back(
    shell: &Rc<Shell>,
    header: gtk::Widget,
) -> gtk::Widget {
    let frame = detail_showcase_frame(header);
    let overlay = gtk::Overlay::new();
    overlay.set_hexpand(true);
    overlay.set_halign(gtk::Align::Fill);
    overlay.set_width_request(1);
    overlay.set_child(Some(&frame));

    let back = icon_button("rufin-go-previous-symbolic", "Back");
    back.add_css_class("detail-back-button");
    back.set_halign(gtk::Align::Start);
    back.set_valign(gtk::Align::Start);
    back.set_margin_top(1);
    back.set_margin_start(4);
    back.set_sensitive(shell.navigation.routes.borrow().can_back());
    {
        let shell = Rc::clone(shell);
        back.connect_clicked(move |_| shell.go_back());
    }
    overlay.add_overlay(&back);
    overlay.set_measure_overlay(&back, false);
    overlay.upcast()
}

pub(crate) fn mark_tiny_detail_showcase(widget: &impl IsA<gtk::Widget>, width: i32) {
    update_tiny_detail_showcase(widget, width);
}

fn update_tiny_detail_showcase(widget: &impl IsA<gtk::Widget>, width: i32) {
    if width < 520 {
        widget.add_css_class("detail-showcase-tiny");
    } else {
        widget.remove_css_class("detail-showcase-tiny");
    }
    if detail_showcase_cover_only(width) {
        widget.add_css_class("detail-showcase-cover-only");
    } else {
        widget.remove_css_class("detail-showcase-cover-only");
    }
}

pub(crate) fn fit_detail_text(label: &gtk::Label, text: &str) {
    let count = text.chars().count();
    if count >= 42 {
        label.add_css_class("detail-text-very-long");
    } else if count >= 24 {
        label.add_css_class("detail-text-long");
    }
}

pub(crate) fn album_external_links(shell: &Rc<Shell>, album: &Album) -> Option<gtk::Widget> {
    let settings = shell.settings.current.borrow();
    let link_settings = &settings.external_site_links;
    if !settings.shows_external_site_links() {
        return None;
    }

    let row = detail_external_link_row();
    if link_settings.lastfm
        && let Some(url) = lastfm_album_url(&album.artist, &album.title)
    {
        row.append(&detail_external_link_button(
            shell,
            "io.github.screwys.Rufin.external.lastfm",
            msgid("Open on Last.fm"),
            url,
        ));
    }
    if link_settings.musicbrainz
        && let Some(url) = musicbrainz_album_url(album)
    {
        row.append(&detail_external_link_button(
            shell,
            "io.github.screwys.Rufin.external.musicbrainz",
            msgid("Open on MusicBrainz"),
            url,
        ));
    }
    if link_settings.server
        && let Some(link) = server_entity_url(shell, DetailEntityKind::Album, album.id.as_str())
    {
        row.append(&detail_external_link_button(
            shell,
            link.icon_name,
            link.label,
            link.url,
        ));
    }

    row.first_child().is_some().then(|| row.upcast())
}

pub(crate) fn artist_external_links(shell: &Rc<Shell>, artist: &Artist) -> Option<gtk::Widget> {
    let settings = shell.settings.current.borrow();
    let link_settings = &settings.external_site_links;
    if !settings.shows_external_site_links() {
        return None;
    }

    let row = detail_external_link_row();
    if link_settings.lastfm
        && let Some(url) = lastfm_artist_url(&artist.name)
    {
        row.append(&detail_external_link_button(
            shell,
            "io.github.screwys.Rufin.external.lastfm",
            msgid("Open on Last.fm"),
            url,
        ));
    }
    if link_settings.musicbrainz
        && let Some(url) = musicbrainz_artist_url(artist)
    {
        row.append(&detail_external_link_button(
            shell,
            "io.github.screwys.Rufin.external.musicbrainz",
            msgid("Open on MusicBrainz"),
            url,
        ));
    }
    if link_settings.server
        && let Some(link) = server_entity_url(shell, DetailEntityKind::Artist, artist.id.as_str())
    {
        row.append(&detail_external_link_button(
            shell,
            link.icon_name,
            link.label,
            link.url,
        ));
    }

    row.first_child().is_some().then(|| row.upcast())
}

fn detail_external_link_row() -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.add_css_class("detail-external-link-row");
    row
}

fn detail_external_link_button(
    shell: &Rc<Shell>,
    icon_name: &str,
    label: &str,
    url: String,
) -> gtk::Button {
    let button = gtk::Button::new();
    button.add_css_class("icon-button");
    button.add_css_class("flat");
    button.add_css_class("circular");
    button.add_css_class("detail-external-link-button");
    button.set_tooltip_text(Some(&tr(label)));
    let image = gtk::Image::from_icon_name(icon_name);
    image.set_pixel_size(18);
    button.set_child(Some(&image));
    let window = shell.chrome.window.clone();
    button.connect_clicked(move |_| {
        let launcher = gtk::UriLauncher::new(&url);
        let window = window.clone();
        gtk::glib::spawn_future_local(async move {
            if let Err(error) = launcher.launch_future(Some(&window)).await {
                warn!(%error, "failed to open external detail link");
            }
        });
    });
    button
}

fn lastfm_album_url(artist: &str, album: &str) -> Option<String> {
    let artist = clean_url_label(artist)?;
    let album = clean_url_label(album)?;
    Some(format!(
        "https://www.last.fm/music/{}/{}",
        percent_encode_path_segment(artist),
        percent_encode_path_segment(album)
    ))
}

fn lastfm_artist_url(artist: &str) -> Option<String> {
    let artist = clean_url_label(artist)?;
    Some(format!(
        "https://www.last.fm/music/{}",
        percent_encode_path_segment(artist)
    ))
}

fn musicbrainz_album_url(album: &Album) -> Option<String> {
    if let Some(group_id) = album
        .musicbrainz_release_group_id
        .as_deref()
        .and_then(clean_url_label)
    {
        return Some(format!("https://musicbrainz.org/release-group/{group_id}"));
    }
    let release_id = album
        .musicbrainz_album_id
        .as_deref()
        .and_then(clean_url_label)?;
    Some(format!("https://musicbrainz.org/release/{release_id}"))
}

fn musicbrainz_artist_url(artist: &Artist) -> Option<String> {
    let artist_id = artist
        .musicbrainz_artist_id
        .as_deref()
        .and_then(clean_url_label)?;
    Some(format!("https://musicbrainz.org/artist/{artist_id}"))
}

fn server_entity_url(
    shell: &Shell,
    kind: DetailEntityKind,
    entity_id: &str,
) -> Option<DetailExternalLink> {
    let source_id = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_id.clone())?;
    let source = shell
        .products
        .source
        .configured_source(&source_id)
        .ok()
        .flatten()?;
    server_entity_link(
        &source.source.kind,
        &source.credentials.server_url,
        kind,
        entity_id,
    )
}

fn clean_url_label(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn percent_encode_path_segment(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(*byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{byte:02X}"));
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{
        detail_cover_render_size, detail_external_links_visible, full_artwork_size,
        lastfm_album_url, lastfm_artist_url, musicbrainz_album_url, summary_value_is_visible,
    };
    use crate::routes::route_layout::detail_showcase_cover_size;

    #[test]
    fn responsive_detail_artwork_covers_every_presented_size() {
        let render_size = detail_cover_render_size();
        for width in 1..=1_200 {
            assert!(detail_showcase_cover_size(width) <= render_size);
        }
    }

    #[test]
    fn detail_external_links_need_content_and_metadata_space() {
        assert!(!detail_external_links_visible(false, false));
        assert!(!detail_external_links_visible(false, true));
        assert!(!detail_external_links_visible(true, true));
        assert!(detail_external_links_visible(true, false));
    }

    #[test]
    fn empty_summary_values_hide_the_icon_and_text() {
        assert!(!summary_value_is_visible(""));
        assert!(summary_value_is_visible("1 track"));
    }

    #[test]
    fn full_artwork_size_fits_window() {
        assert_eq!(full_artwork_size(1440, 900), 720);
        assert_eq!(full_artwork_size(640, 480), 400);
        assert_eq!(full_artwork_size(300, 260), 240);
    }

    #[test]
    fn lastfm_urls_escape_path_segments() {
        assert_eq!(
            lastfm_album_url("Test Artist", "A/B").as_deref(),
            Some("https://www.last.fm/music/Test%20Artist/A%2FB")
        );
        assert_eq!(
            lastfm_artist_url("青葉市子").as_deref(),
            Some("https://www.last.fm/music/%E9%9D%92%E8%91%89%E5%B8%82%E5%AD%90")
        );
    }

    #[test]
    fn musicbrainz_album_url_prefers_release_group() {
        let mut album = crate::test_support::album(1, "Album");
        album.musicbrainz_album_id = Some("release-one".to_string());
        album.musicbrainz_release_group_id = Some("group-one".to_string());

        assert_eq!(
            musicbrainz_album_url(&album).as_deref(),
            Some("https://musicbrainz.org/release-group/group-one")
        );

        album.musicbrainz_release_group_id = None;
        assert_eq!(
            musicbrainz_album_url(&album).as_deref(),
            Some("https://musicbrainz.org/release/release-one")
        );
    }
}
