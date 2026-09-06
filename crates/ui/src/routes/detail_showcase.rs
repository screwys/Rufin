use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use ::library::{AlbumRow, ArtistRow};
use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::subclass::prelude::ObjectSubclassIsExt;
use gtk::{CompositeTemplate, TemplateChild, glib, subclass::prelude::*};
use tracing::warn;

use crate::interactions::{ContextMenuOpen, RADIO_ICON, add_widget_click};
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
    CoverHoverControls, cover_hover_controls, cover_play_hover_controls, showcase_cover_overlay,
};
use super::collections::CollectionPlay;
use super::detail_links::{DetailEntityKind, DetailExternalLink, server_entity_link};
use super::route::Route;
use super::route_layout::{detail_showcase_cover_only, detail_showcase_cover_size};

const DETAIL_HEADER_SPACING: i32 = 18;
mod showcase_imp {
    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/screwys/Rufin/ui/routes/detail_showcase.ui")]
    pub(crate) struct DetailShowcaseView {
        #[template_child]
        pub(super) cover_column: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) cover_host: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) external_links: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) metadata: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) text_stack: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) kind_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) kind: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) kind_controls: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) title: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) details: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) summary: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) summary_item_0: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) summary_icon_0: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) summary_label_0: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) summary_item_1: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) summary_icon_1: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) summary_label_1: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) summary_item_2: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) summary_icon_2: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) summary_label_2: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) actions: TemplateChild<gtk::Box>,
        pub(super) cover_only: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DetailShowcaseView {
        const NAME: &'static str = "RufinDetailShowcaseView";
        type Type = super::DetailShowcaseView;
        type ParentType = gtk::Box;

        fn class_init(class: &mut Self::Class) {
            class.bind_template();
        }

        fn instance_init(instance: &glib::subclass::InitializingObject<Self>) {
            instance.init_template();
        }
    }

    impl ObjectImpl for DetailShowcaseView {
        fn dispose(&self) {
            self.dispose_template();
        }
    }
    impl WidgetImpl for DetailShowcaseView {}
    impl BoxImpl for DetailShowcaseView {}
}

glib::wrapper! {
    pub(crate) struct DetailShowcaseView(ObjectSubclass<showcase_imp::DetailShowcaseView>)
        @extends gtk::Widget, gtk::Box,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl DetailShowcaseView {
    pub(crate) fn new(
        route_class: &'static str,
        seed: u32,
        kind: &'static str,
        genre_kind: bool,
        title: &str,
    ) -> Self {
        let view: Self = glib::Object::new();
        view.add_css_class("detail-showcase");
        view.add_css_class(route_class);
        add_album_seed_gradient_class(&view, seed);
        bind_label_text_with(&view.imp().kind, move || tr(kind));
        if genre_kind {
            view.imp().kind_row.add_css_class("album-detail-genre-row");
        }
        view.set_title(title);
        view
    }

    pub(crate) fn set_title(&self, text: &str) {
        self.imp().title.set_text(text);
        self.imp().title.remove_css_class("detail-text-long");
        self.imp().title.remove_css_class("detail-text-very-long");
        fit_detail_text(&self.imp().title, text);
    }

    pub(crate) fn append_kind_control(&self, child: &impl IsA<gtk::Widget>) {
        self.imp().kind_controls.append(child);
    }

    pub(crate) fn clear_kind_controls(&self) {
        while let Some(child) = self.imp().kind_controls.first_child() {
            self.imp().kind_controls.remove(&child);
        }
    }

    pub(crate) fn append_detail(&self, child: &impl IsA<gtk::Widget>) {
        self.imp().details.set_visible(true);
        self.imp().details.append(child);
    }

    pub(crate) fn actions(&self) -> gtk::Box {
        self.imp().actions.get()
    }

    pub(crate) fn replace_summary(&self, values: &[(&str, String)]) {
        let items = [
            (
                self.imp().summary_item_0.get(),
                self.imp().summary_icon_0.get(),
                self.imp().summary_label_0.get(),
            ),
            (
                self.imp().summary_item_1.get(),
                self.imp().summary_icon_1.get(),
                self.imp().summary_label_1.get(),
            ),
            (
                self.imp().summary_item_2.get(),
                self.imp().summary_icon_2.get(),
                self.imp().summary_label_2.get(),
            ),
        ];
        let mut any = false;
        for (index, (item, icon, label)) in items.into_iter().enumerate() {
            if let Some((icon_name, text)) = values
                .get(index)
                .filter(|(_, text)| summary_value_is_visible(text))
            {
                icon.set_icon_name(Some(icon_name));
                label.set_text(text);
                item.set_visible(true);
                any = true;
            } else {
                item.set_visible(false);
            }
        }
        self.imp().summary.set_visible(any);
    }

    pub(crate) fn bind_summary_text_with(&self, index: usize, text: impl Fn() -> String + 'static) {
        let labels = [
            self.imp().summary_label_0.get(),
            self.imp().summary_label_1.get(),
            self.imp().summary_label_2.get(),
        ];
        if let Some(label) = labels.get(index) {
            bind_label_text_with(label, text);
        }
    }

    pub(crate) fn replace_external_links(&self, links: Option<gtk::Widget>) {
        while let Some(child) = self.imp().external_links.first_child() {
            self.imp().external_links.remove(&child);
        }
        if let Some(links) = links {
            links.set_halign(gtk::Align::Center);
            self.imp().external_links.append(&links);
        }
        self.apply_external_links_visibility();
    }

    pub(crate) fn add_external_links_class(&self, class: &'static str) {
        self.imp().external_links.add_css_class(class);
    }

    fn attach_cover(&self, cover: &impl IsA<gtk::Widget>) {
        self.imp().cover_host.append(cover);
    }

    fn set_cover_only(&self, cover_only: bool) {
        self.imp().metadata.set_visible(!cover_only);
        self.imp().cover_only.set(cover_only);
        self.apply_external_links_visibility();
    }

    fn apply_external_links_visibility(&self) {
        self.imp()
            .external_links
            .set_visible(detail_external_links_visible(
                self.imp().external_links.first_child().is_some(),
                self.imp().cover_only.get(),
            ));
    }

    fn set_media_layout(&self) {
        self.add_css_class("detail-showcase-horizontal");
        self.set_spacing(DETAIL_HEADER_SPACING);
        self.imp().metadata.set_valign(gtk::Align::Start);
        self.imp().text_stack.set_spacing(8);
    }

    fn set_collection_layout(&self, spacing: i32) {
        self.add_css_class("playlist-detail-showcase");
        self.set_spacing(spacing);
        self.imp().metadata.set_valign(gtk::Align::Center);
        self.imp().text_stack.set_spacing(10);
    }
}

pub(crate) struct MediaShowcase {
    pub(crate) view: DetailShowcaseView,
    pub(crate) initial_width: i32,
    pub(crate) cover: MediaCoverProjection,
    pub(crate) cover_controls: CoverHoverControls,
    pub(crate) context_menu: Option<ContextMenuOpen>,
    pub(crate) actions_min_cover_size: Option<i32>,
}

fn detail_external_links_visible(has_links: bool, cover_only: bool) -> bool {
    has_links && !cover_only
}

pub(crate) struct CollectionDetailShowcase {
    pub(crate) view: DetailShowcaseView,
    pub(crate) initial_width: i32,
    pub(crate) compact_spacing: i32,
    pub(crate) wide_spacing: i32,
    pub(crate) cover: CoverGroupProjection,
    pub(crate) cover_controls: CoverHoverControls,
    pub(crate) context_menu: Option<ContextMenuOpen>,
}

pub(crate) fn media_showcase(config: MediaShowcase) -> gtk::Widget {
    let view = config.view;
    view.set_media_layout();
    view.attach_cover(&showcase_cover_overlay(
        config.cover.button().upcast_ref(),
        config.cover_controls,
        config.context_menu,
    ));
    let adaptive_actions = config
        .actions_min_cover_size
        .map(|threshold| (view.actions().upcast(), threshold));
    let presentation = MediaShowcasePresentation {
        viewport_width: Cell::new(0),
        cover_width: Cell::new(0),
        view: view.clone(),
        cover: config.cover,
        adaptive_actions,
    };
    presentation.apply_viewport_width(config.initial_width);
    presentation.resize_cover(config.initial_width);
    let frame = detail_showcase_frame(view.upcast());
    width_allocation_owner(&frame, move |width| {
        presentation.apply_viewport_width(width);
        presentation.resize_cover(width);
    })
    .upcast()
}

struct MediaShowcasePresentation {
    viewport_width: Cell<i32>,
    cover_width: Cell<i32>,
    view: DetailShowcaseView,
    cover: MediaCoverProjection,
    adaptive_actions: Option<(gtk::Widget, i32)>,
}

impl MediaShowcasePresentation {
    fn apply_viewport_width(&self, width: i32) {
        if width <= 1 || self.viewport_width.replace(width) == width {
            return;
        }
        let cover_only = detail_showcase_cover_only(width);
        update_tiny_detail_showcase(&self.view, width);
        self.view.set_cover_only(cover_only);
    }

    fn resize_cover(&self, width: i32) {
        if width <= 1 || self.cover_width.replace(width) == width {
            return;
        }
        let cover_size = super::route_layout::detail_showcase_cover_size(width);
        self.view.imp().cover_column.set_width_request(cover_size);
        self.cover.resize(cover_size);
        if let Some((actions, threshold)) = self.adaptive_actions.as_ref() {
            actions.set_visible(!detail_showcase_cover_only(width) && cover_size >= *threshold);
        }
    }
}

pub(crate) fn collection_detail_showcase(
    shell: &Rc<Shell>,
    config: CollectionDetailShowcase,
) -> gtk::Widget {
    let view = config.view;
    view.set_collection_layout(config.wide_spacing);
    view.attach_cover(&showcase_cover_overlay(
        &config.cover.widget(),
        config.cover_controls,
        config.context_menu,
    ));

    let presentation = CollectionShowcasePresentation {
        viewport_width: Cell::new(0),
        cover_width: Cell::new(0),
        view: view.clone(),
        cover: config.cover,
        compact_spacing: config.compact_spacing,
        wide_spacing: config.wide_spacing,
    };
    presentation.apply_viewport_width(config.initial_width);
    presentation.resize_cover(config.initial_width);
    let frame = detail_showcase_frame_with_back(shell, view.upcast());
    width_allocation_owner(&frame, move |width| {
        presentation.apply_viewport_width(width);
        presentation.resize_cover(width);
    })
    .upcast()
}

struct CollectionShowcasePresentation {
    viewport_width: Cell<i32>,
    cover_width: Cell<i32>,
    view: DetailShowcaseView,
    cover: CoverGroupProjection,
    compact_spacing: i32,
    wide_spacing: i32,
}

impl CollectionShowcasePresentation {
    fn apply_viewport_width(&self, width: i32) {
        if width <= 1 || self.viewport_width.replace(width) == width {
            return;
        }
        let cover_only = detail_showcase_cover_only(width);
        update_tiny_detail_showcase(&self.view, width);
        self.view.set_spacing(
            if super::playlist_detail::playlist_detail_compact_for_width(width) {
                self.compact_spacing
            } else {
                self.wide_spacing
            },
        );
        self.view.set_cover_only(cover_only);
    }

    fn resize_cover(&self, width: i32) {
        if width <= 1 || self.cover_width.replace(width) == width {
            return;
        }
        let cover_size = super::playlist_detail::playlist_cover_size(width);
        self.cover.resize(cover_size);
    }
}

fn summary_value_is_visible(text: &str) -> bool {
    !text.is_empty()
}

pub(crate) fn detail_action_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = icon_button(icon_name, label);
    configure_action_button(&button, ActionButtonVariant::DetailAction);
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
    configure_action_button(&button, ActionButtonVariant::DetailAction);
    button
}

pub(crate) fn detail_primary_action_button(icon_name: &str, label: &str) -> gtk::Button {
    let button = icon_button(icon_name, label);
    configure_action_button(&button, ActionButtonVariant::DetailPrimary);
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
    connect_collection_play(&primary, Rc::clone(&play), playback::QueuePlacement::Now);
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
            connect_collection_play(&button, Rc::clone(&play), placement);
            actions.append(&button);
        }
        connect_collection_play(&hover, Rc::clone(&play), placement);
    }
    connect_collection_play(&controls.play, play, playback::QueuePlacement::Now);
    controls
}

fn connect_collection_play(
    button: &gtk::Button,
    play: CollectionPlay,
    placement: playback::QueuePlacement,
) {
    button.connect_clicked(move |_| play(placement));
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

#[derive(Clone)]
pub(crate) struct MediaCoverProjection {
    button: gtk::Button,
    tile: ArtworkTile,
    size: Rc<Cell<i32>>,
    candidates: Rc<RefCell<ArtworkBinding>>,
}

impl MediaCoverProjection {
    pub(crate) fn button(&self) -> gtk::Button {
        self.button.clone()
    }

    pub(crate) fn drag_paintable_source(&self) -> gtk::Picture {
        self.tile.drag_paintable_source()
    }

    pub(crate) fn resize(&self, size: i32) {
        let size = size.max(1);
        if self.size.replace(size) == size {
            return;
        }
        self.button.set_size_request(size, size);
        self.tile.set_square_size(size);
    }

    pub(crate) fn replace(&self, shell: &Rc<Shell>, candidates: ArtworkBinding) {
        self.candidates.replace(candidates.clone());
        shell.bind_artwork_tile(
            &self.tile,
            candidates,
            detail_cover_render_size(),
            LARGE_COVER_SIZE,
        );
    }
}

pub(crate) fn media_cover_projection(
    shell: &Rc<Shell>,
    candidates: ArtworkBinding,
    size: i32,
    cover_class: &str,
) -> MediaCoverProjection {
    media_cover_projection_with_open(shell, candidates, size, cover_class, |shell, candidates| {
        shell.present_full_artwork(candidates)
    })
}

pub(crate) fn home_album_cover_projection(
    shell: &Rc<Shell>,
    album: &AlbumRow,
    size: i32,
) -> MediaCoverProjection {
    let album_uri = album.media_uri.clone();
    media_cover_projection_with_open(
        shell,
        album
            .artwork_binding
            .as_deref()
            .map(ArtworkBinding::opaque)
            .unwrap_or_default(),
        size,
        "",
        move |shell, _| shell.navigate(Route::AlbumDetail(album_uri.clone())),
    )
}

fn media_cover_projection_with_open(
    shell: &Rc<Shell>,
    candidates: ArtworkBinding,
    size: i32,
    cover_class: &str,
    open: impl Fn(&Rc<Shell>, ArtworkBinding) + 'static,
) -> MediaCoverProjection {
    let render_size = detail_cover_render_size();
    let fetch_size = LARGE_COVER_SIZE;
    let tile = ArtworkTile::new_sized(size, size);
    shell.bind_artwork_tile(&tile, candidates.clone(), render_size, fetch_size);
    let cover = tile.widget();
    cover.add_css_class("detail-showcase-cover");
    if !cover_class.is_empty() {
        cover.add_css_class(cover_class);
    }

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
        open(&shell, candidates);
    });
    MediaCoverProjection {
        button,
        tile,
        size: Rc::new(Cell::new(size)),
        candidates,
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

pub(crate) fn album_external_links(shell: &Rc<Shell>, album: &AlbumRow) -> Option<gtk::Widget> {
    let settings = shell.settings.current.borrow();
    let link_settings = &settings.external_site_links;
    if !settings.shows_external_site_links() {
        return None;
    }

    let row = detail_external_link_row();
    if link_settings.lastfm
        && let Some(url) = lastfm_album_url(&album.display_artist, &album.title)
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
        && let Some(link) = server_entity_url(shell, DetailEntityKind::Album, &album.object_id)
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

pub(crate) fn artist_external_links(shell: &Rc<Shell>, artist: &ArtistRow) -> Option<gtk::Widget> {
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
        && let Some(link) = server_entity_url(shell, DetailEntityKind::Artist, &artist.object_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responsive_detail_artwork_covers_every_presented_size() {
        let render = detail_cover_render_size();
        for width in [240, 360, 520, 760, 1_200, i32::MAX] {
            assert!(render >= detail_showcase_cover_size(width));
        }
    }

    #[test]
    fn detail_external_links_need_content_and_metadata_space() {
        assert!(detail_external_links_visible(true, false));
        assert!(!detail_external_links_visible(false, false));
        assert!(!detail_external_links_visible(true, true));
    }

    #[test]
    fn empty_summary_values_hide_the_icon_and_text() {
        assert!(!summary_value_is_visible(""));
        assert!(summary_value_is_visible("0"));
    }

    #[test]
    fn full_artwork_size_fits_window() {
        assert_eq!(full_artwork_size(300, 300), 240);
        assert_eq!(full_artwork_size(4_000, 3_000), 720);
        assert!(full_artwork_size(900, 700) <= 700);
    }
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

fn musicbrainz_album_url(album: &AlbumRow) -> Option<String> {
    if let Some(group_id) = album
        .musicbrainz_release_group_id
        .as_deref()
        .and_then(clean_url_label)
    {
        return Some(format!("https://musicbrainz.org/release-group/{group_id}"));
    }
    let release_id = album
        .musicbrainz_release_id
        .as_deref()
        .and_then(clean_url_label)?;
    Some(format!("https://musicbrainz.org/release/{release_id}"))
}

fn musicbrainz_artist_url(artist: &ArtistRow) -> Option<String> {
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
