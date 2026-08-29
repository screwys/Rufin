use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use artwork::ArtworkBinding;
use gtk::glib;
use playback::{CurrentMedia, EQUALIZER_BAND_COUNT, EqualizerSettings, PlaybackView};

use crate::layout::allocation_owner;
use crate::shell::Shell;
use crate::shell::actions::icon_button;
use crate::shell::chrome::top_window_drag_handle;
use crate::shell::cover::ArtworkTile;
use crate::shell::cover::cover_fetch_size_for_display;
use crate::shell::layout::MIN_USEFUL_MAIN_WIDTH;
use localization::tr;

use super::bottom::BOTTOM_PLAYER_HEIGHT;
use super::equalizer::{
    build_equalizer_preset_dropdown, connect_equalizer_scale_commit, equalizer_band_label_parts,
    equalizer_band_title, equalizer_default_preset_bands, equalizer_preset_bands,
    equalizer_preset_name_at, equalizer_preset_position, equalizer_selected_preset,
    install_equalizer_scroll, relocalize_equalizer_preset_dropdown,
};
use super::icons::lyrics_icon_area;

const FULLSCREEN_PLAYER_OPEN_TRANSITION_MS: u32 = 420;
const FULLSCREEN_PLAYER_CLOSE_TRANSITION_MS: u32 = 320;
const FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE: i32 = 320;
const FULLSCREEN_PLAYER_MIN_COVER_SIZE: i32 = 140;
const FULLSCREEN_PLAYER_MAX_COVER_SIZE: i32 = 320;
const FULLSCREEN_PLAYER_FALLBACK_HORIZONTAL_RESERVED: i32 = 186;
const FULLSCREEN_PLAYER_HERO_SPACING: i32 = 18;
const FULLSCREEN_PLAYER_VERTICAL_RESERVED: i32 = 430;
const FULLSCREEN_PLAYER_HERO_MIN_WINDOW_HEIGHT: i32 = 560;
const FULLSCREEN_ICON_SIZE: i32 = 18;
const FULLSCREEN_EQUALIZER_MIN_SCALE_HEIGHT: i32 = 124;
const FULLSCREEN_EQUALIZER_MAX_SCALE_HEIGHT: i32 = 286;
const FULLSCREEN_EQUALIZER_ROW_HEIGHT: i32 = 240;
const FULLSCREEN_EQUALIZER_TINY_ROW_HEIGHT: i32 =
    FULLSCREEN_EQUALIZER_MIN_SCALE_HEIGHT + FULLSCREEN_EQUALIZER_LABEL_HEIGHT;
const FULLSCREEN_EQUALIZER_LEVEL_WIDTH: i32 = 48;
const FULLSCREEN_EQUALIZER_ROW_GAP: i32 = 16;
const FULLSCREEN_EQUALIZER_MIN_BAND_WIDTH: i32 = 14;
const FULLSCREEN_EQUALIZER_MAX_BAND_WIDTH: i32 = 44;
const FULLSCREEN_EQUALIZER_MIN_BAND_GAP: i32 = 2;
const FULLSCREEN_EQUALIZER_MAX_BAND_GAP: i32 = 10;
const FULLSCREEN_EQUALIZER_LABEL_HEIGHT: i32 = 50;
const FULLSCREEN_EQUALIZER_MIN_FIT_INSET: i32 = 16;
const FULLSCREEN_EQUALIZER_MAX_FIT_INSET: i32 = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FullscreenPlaybackRefresh {
    None,
    Visualizer,
    Static,
}

pub(crate) fn fullscreen_playback_refresh(
    previous: Option<&PlaybackView>,
    next: &PlaybackView,
) -> FullscreenPlaybackRefresh {
    let Some(previous) = previous else {
        return FullscreenPlaybackRefresh::Static;
    };
    if previous.transport.source_id != next.transport.source_id
        || previous.transport.current != next.transport.current
    {
        FullscreenPlaybackRefresh::Static
    } else if previous.transport.effective_state() != next.transport.effective_state() {
        FullscreenPlaybackRefresh::Visualizer
    } else {
        FullscreenPlaybackRefresh::None
    }
}

pub(crate) struct FullscreenPlayerParts {
    pub(crate) root: gtk::Overlay,
    pub(crate) visible: Cell<bool>,
    pub(crate) animation_tick: RefCell<Option<gtk::TickCallbackId>>,
    pub(crate) close_button: gtk::Button,
    pub(crate) inline_close_button: gtk::Button,
    hero: gtk::Box,
    hero_content: gtk::Box,
    cover: ArtworkTile,
    title: gtk::Label,
    artist: gtk::Label,
    album: gtk::Label,
    meta: gtk::FlowBox,
    pub(crate) stack: adw::ViewStack,
    pub(crate) tabs: Vec<(gtk::ToggleButton, gtk::Label, &'static str)>,
    pub(crate) lyrics_host: gtk::Box,
    pub(crate) queue_panel: gtk::Box,
    pub(crate) visualizer_panel: gtk::Box,
    pub(crate) equalizer_panel: gtk::ScrolledWindow,
    equalizer_enabled: gtk::Switch,
    pub(crate) equalizer_enabled_label: gtk::Label,
    equalizer_scales: Vec<gtk::Scale>,
    pub(crate) equalizer_reset_button: gtk::Button,
    pub(crate) equalizer_preset_label: gtk::Label,
    equalizer_preset_dropdown: gtk::DropDown,
    equalizer_band_row: gtk::Box,
    equalizer_syncing: Rc<Cell<bool>>,
}

struct EqualizerPanel {
    root: gtk::ScrolledWindow,
    enabled: gtk::Switch,
    enabled_label: gtk::Label,
    scales: Vec<gtk::Scale>,
    reset_button: gtk::Button,
    preset_label: gtk::Label,
    preset_dropdown: gtk::DropDown,
    band_row: gtk::Box,
    syncing: Rc<Cell<bool>>,
}

pub(crate) fn build_fullscreen_player(
    hero_start_controls: &impl IsA<gtk::Widget>,
    hero_end_controls: &impl IsA<gtk::Widget>,
    inline_start_controls: &impl IsA<gtk::Widget>,
    inline_end_controls: &impl IsA<gtk::Widget>,
    visualizer_area: &gtk::DrawingArea,
) -> FullscreenPlayerParts {
    let root = gtk::Overlay::new();
    root.add_css_class("fullscreen-player");
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_visible(false);
    root.set_can_target(false);
    root.set_sensitive(false);
    root.set_opacity(0.0);

    let close_button = icon_button("rufin-go-down-symbolic", "Close fullscreen player");
    close_button.add_css_class("fullscreen-player-close-button");
    close_button.set_valign(gtk::Align::Start);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.add_css_class("fullscreen-player-body");
    body.set_hexpand(true);
    body.set_vexpand(true);

    let hero = gtk::Box::new(gtk::Orientation::Horizontal, FULLSCREEN_PLAYER_HERO_SPACING);
    hero.add_css_class("fullscreen-player-hero");
    hero.set_halign(gtk::Align::Fill);
    hero.set_valign(gtk::Align::Center);
    hero.set_hexpand(true);
    hero.set_width_request(1);
    hero.append(hero_start_controls);
    hero.append(&close_button);

    let hero_content = gtk::Box::new(gtk::Orientation::Horizontal, FULLSCREEN_PLAYER_HERO_SPACING);
    hero_content.set_width_request(1);
    hero_content.set_hexpand(true);
    hero_content.set_halign(gtk::Align::Fill);
    hero_content.set_valign(gtk::Align::Center);

    let cover = ArtworkTile::new(FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE);
    cover.area.add_css_class("fullscreen-player-cover");
    cover.area.set_halign(gtk::Align::End);
    hero_content.append(&cover.area);

    let details = gtk::Box::new(gtk::Orientation::Vertical, 5);
    details.add_css_class("fullscreen-player-details");
    details.set_hexpand(true);
    details.set_width_request(1);
    details.set_halign(gtk::Align::Fill);
    details.set_valign(gtk::Align::Center);

    let title = fullscreen_player_label("fullscreen-player-title");
    let artist = fullscreen_player_label("fullscreen-player-artist");
    let album = fullscreen_player_label("fullscreen-player-album");
    let meta = fullscreen_player_meta_row();
    for label in [&title, &artist, &album] {
        label.set_halign(gtk::Align::Start);
        label.set_xalign(0.0);
        label.set_justify(gtk::Justification::Left);
    }
    details.append(&title);
    details.append(&artist);
    details.append(&album);
    details.append(&meta);
    hero_content.append(&details);
    hero.append(&hero_content);
    hero.append(hero_end_controls);
    body.append(&hero);

    let stack = adw::ViewStack::builder()
        .hhomogeneous(false)
        .vhomogeneous(false)
        .hexpand(true)
        .vexpand(true)
        .build();

    let queue_panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
    queue_panel.add_css_class("fullscreen-player-pane");
    queue_panel.add_css_class("fullscreen-player-queue-panel");
    queue_panel.set_hexpand(true);
    queue_panel.set_vexpand(true);
    stack.add_titled(&queue_panel, Some("queue"), &tr("Queue"));

    let lyrics_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    lyrics_host.add_css_class("fullscreen-player-pane");
    lyrics_host.set_hexpand(true);
    lyrics_host.set_vexpand(true);
    stack.add_titled(&lyrics_host, Some("lyrics"), &tr("Lyrics"));

    let visualizer_panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    visualizer_panel.add_css_class("fullscreen-player-pane");
    visualizer_panel.add_css_class("fullscreen-player-visualizer");
    visualizer_panel.set_hexpand(true);
    visualizer_panel.set_vexpand(true);
    visualizer_panel.append(visualizer_area);
    stack.add_titled(&visualizer_panel, Some("visualizer"), &tr("Visualizer"));

    let equalizer = build_fullscreen_equalizer_panel();
    stack.add_titled(&equalizer.root, Some("equalizer"), &tr("Equalizer"));
    stack.set_visible_child_name("lyrics");

    let switcher_bar = gtk::CenterBox::new();
    switcher_bar.add_css_class("fullscreen-player-tab-bar");
    switcher_bar.set_margin_start(14);
    switcher_bar.set_hexpand(true);
    let inline_close_button = icon_button("rufin-go-down-symbolic", "Close fullscreen player");
    inline_close_button.add_css_class("fullscreen-player-close-button");
    inline_close_button.set_visible(false);
    let inline_start = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    inline_start.append(inline_start_controls);
    inline_start.append(&inline_close_button);
    switcher_bar.set_start_widget(Some(&inline_start));
    switcher_bar.set_end_widget(Some(inline_end_controls));
    let (switcher, tabs) = fullscreen_player_switcher(&stack);
    switcher_bar.set_center_widget(Some(&switcher));
    body.append(&switcher_bar);
    body.append(&stack);
    root.set_child(Some(&body));
    let drag_handle = top_window_drag_handle("fullscreen-player-drag-handle");
    root.add_overlay(&drag_handle);
    root.set_measure_overlay(&drag_handle, false);

    FullscreenPlayerParts {
        root,
        visible: Cell::new(false),
        animation_tick: RefCell::new(None),
        close_button,
        inline_close_button,
        hero,
        hero_content,
        cover,
        title,
        artist,
        album,
        meta,
        stack,
        tabs,
        lyrics_host,
        queue_panel,
        visualizer_panel,
        equalizer_panel: equalizer.root.clone(),
        equalizer_enabled: equalizer.enabled,
        equalizer_enabled_label: equalizer.enabled_label,
        equalizer_scales: equalizer.scales,
        equalizer_reset_button: equalizer.reset_button,
        equalizer_preset_label: equalizer.preset_label,
        equalizer_preset_dropdown: equalizer.preset_dropdown,
        equalizer_band_row: equalizer.band_row,
        equalizer_syncing: equalizer.syncing,
    }
}

fn fullscreen_player_switcher(
    stack: &adw::ViewStack,
) -> (gtk::Box, Vec<(gtk::ToggleButton, gtk::Label, &'static str)>) {
    let switcher = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    switcher.add_css_class("linked");
    switcher.set_halign(gtk::Align::Center);

    let (queue, queue_label) = fullscreen_player_tab_button(
        gtk::Image::from_icon_name("rufin-view-list-ordered-symbolic").upcast(),
        "Queue",
    );
    let (lyrics, lyrics_label) = fullscreen_player_tab_button(
        lyrics_icon_area(Rc::new(Cell::new(true))).upcast(),
        "Lyrics",
    );
    let (visualizer, visualizer_label) =
        fullscreen_player_tab_button(fullscreen_visualizer_icon().upcast(), "Visualizer");
    let (equalizer, equalizer_label) =
        fullscreen_player_tab_button(fullscreen_equalizer_icon().upcast(), "Equalizer");
    lyrics.set_active(true);

    let queue_stack = stack.clone();
    queue.connect_clicked(move |_| {
        queue_stack.set_visible_child_name("queue");
    });
    let lyrics_stack = stack.clone();
    lyrics.connect_clicked(move |_| {
        lyrics_stack.set_visible_child_name("lyrics");
    });
    let visualizer_stack = stack.clone();
    visualizer.connect_clicked(move |_| {
        visualizer_stack.set_visible_child_name("visualizer");
    });
    let equalizer_stack = stack.clone();
    equalizer.connect_clicked(move |_| {
        equalizer_stack.set_visible_child_name("equalizer");
    });

    let visualizer_for_notify = visualizer.clone();
    let lyrics_for_notify = lyrics.clone();
    let queue_for_notify = queue.clone();
    let equalizer_for_notify = equalizer.clone();
    stack.connect_visible_child_name_notify(move |stack| {
        let page = stack.visible_child_name();
        visualizer_for_notify.set_active(page.as_deref() == Some("visualizer"));
        lyrics_for_notify.set_active(page.as_deref() == Some("lyrics"));
        queue_for_notify.set_active(page.as_deref() == Some("queue"));
        equalizer_for_notify.set_active(page.as_deref() == Some("equalizer"));
    });

    switcher.append(&queue);
    switcher.append(&lyrics);
    switcher.append(&visualizer);
    switcher.append(&equalizer);
    (
        switcher,
        vec![
            (queue, queue_label, "Queue"),
            (lyrics, lyrics_label, "Lyrics"),
            (visualizer, visualizer_label, "Visualizer"),
            (equalizer, equalizer_label, "Equalizer"),
        ],
    )
}

fn fullscreen_player_tab_button(
    icon: gtk::Widget,
    msgid: &'static str,
) -> (gtk::ToggleButton, gtk::Label) {
    let button = gtk::ToggleButton::new();
    let label_text = tr(msgid);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Center);
    content.set_valign(gtk::Align::Center);
    content.append(&icon);
    let label = gtk::Label::new(Some(&label_text));
    content.append(&label);
    button.set_child(Some(&content));
    button.set_tooltip_text(Some(&label_text));
    button.update_property(&[gtk::accessible::Property::Label(&label_text)]);
    (button, label)
}

fn fullscreen_visualizer_icon() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(FULLSCREEN_ICON_SIZE);
    area.set_content_height(FULLSCREEN_ICON_SIZE);
    area.set_draw_func(|area, context, width, height| {
        let color = area.color();
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            0.9,
        );
        context.set_line_width(1.6);
        let center = f64::from(height) * 0.5;
        let bars = [0.38, 0.74, 0.48, 0.92, 0.56];
        let step = f64::from(width) / (bars.len() + 1) as f64;
        for (index, level) in bars.iter().enumerate() {
            let x = step * (index + 1) as f64;
            let half = f64::from(height) * level * 0.36;
            context.move_to(x, center - half);
            context.line_to(x, center + half);
            let _ = context.stroke();
        }
    });
    area
}

fn fullscreen_equalizer_icon() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(FULLSCREEN_ICON_SIZE);
    area.set_content_height(FULLSCREEN_ICON_SIZE);
    area.set_draw_func(|area, context, width, height| {
        let color = area.color();
        context.set_line_cap(gtk::cairo::LineCap::Round);
        context.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            0.92,
        );
        let width = f64::from(width);
        let height = f64::from(height);
        let tracks = [
            (width * 0.24, height * 0.38),
            (width * 0.5, height * 0.58),
            (width * 0.76, height * 0.38),
        ];
        context.set_line_width(2.2);
        for (x, _) in tracks {
            context.move_to(x, 2.6);
            context.line_to(x, height - 2.6);
            let _ = context.stroke();
        }

        context.set_source_rgba(
            f64::from(color.red()),
            f64::from(color.green()),
            f64::from(color.blue()),
            0.92,
        );
        for (x, y) in tracks {
            context.rectangle(x - 2.4, y - 1.7, 4.8, 3.4);
            let _ = context.fill();
        }
    });
    area
}

fn build_fullscreen_equalizer_panel() -> EqualizerPanel {
    let root = gtk::ScrolledWindow::new();
    root.add_css_class("fullscreen-player-pane");
    root.add_css_class("fullscreen-player-equalizer-scroller");
    root.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Never);
    root.set_hexpand(true);
    root.set_vexpand(true);
    root.set_min_content_width(0);
    root.set_propagate_natural_width(false);
    root.set_propagate_natural_height(false);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 14);
    content.add_css_class("fullscreen-player-equalizer");
    content.set_hexpand(true);
    content.set_vexpand(true);
    content.set_width_request(1);

    let header = gtk::FlowBox::new();
    header.add_css_class("fullscreen-player-equalizer-header");
    header.set_column_spacing(10);
    header.set_row_spacing(6);
    header.set_halign(gtk::Align::Center);
    header.set_valign(gtk::Align::Center);
    header.set_selection_mode(gtk::SelectionMode::None);
    header.set_max_children_per_line(3);

    let enabled_group = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    enabled_group.set_valign(gtk::Align::Center);
    let enabled = gtk::Switch::new();
    enabled.set_valign(gtk::Align::Center);
    let enabled_label = gtk::Label::new(Some(&tr("Enable equalizer")));
    enabled_label.set_valign(gtk::Align::Center);
    enabled_group.append(&enabled_label);
    enabled_group.append(&enabled);
    header.insert(&enabled_group, -1);

    let preset_group = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    preset_group.set_valign(gtk::Align::Center);
    let preset_label = gtk::Label::new(Some(&tr("Preset")));
    preset_label.set_valign(gtk::Align::Center);
    let preset_dropdown = build_equalizer_preset_dropdown(0);
    preset_dropdown.set_valign(gtk::Align::Center);
    preset_group.append(&preset_label);
    preset_group.append(&preset_dropdown);
    header.insert(&preset_group, -1);

    let reset_button = gtk::Button::with_label(&tr("Reset"));
    reset_button.add_css_class("destructive-action");
    header.insert(&reset_button, -1);
    content.append(&header);

    let band_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    band_row.set_halign(gtk::Align::Fill);
    band_row.set_valign(gtk::Align::Fill);
    band_row.set_hexpand(true);
    band_row.set_vexpand(true);
    band_row.set_width_request(1);
    band_row.set_height_request(FULLSCREEN_EQUALIZER_ROW_HEIGHT);
    band_row.set_margin_bottom(6);

    let graph = gtk::Box::new(gtk::Orientation::Horizontal, FULLSCREEN_EQUALIZER_ROW_GAP);
    graph.set_halign(gtk::Align::Center);
    graph.set_valign(gtk::Align::Center);
    graph.set_hexpand(false);
    graph.set_width_request(1);
    let left_levels = fullscreen_equalizer_level_labels(0.0);
    graph.append(&left_levels);

    let bands = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bands.add_css_class("fullscreen-player-equalizer-bands");
    bands.set_halign(gtk::Align::Center);
    bands.set_valign(gtk::Align::Center);
    bands.set_hexpand(false);
    let mut scales = Vec::with_capacity(EQUALIZER_BAND_COUNT);
    let mut band_widgets = Vec::with_capacity(EQUALIZER_BAND_COUNT);
    for index in 0..EQUALIZER_BAND_COUNT {
        let band = gtk::Box::new(gtk::Orientation::Vertical, 6);
        band.set_halign(gtk::Align::Center);
        band.set_valign(gtk::Align::Center);
        band.set_width_request(FULLSCREEN_EQUALIZER_MAX_BAND_WIDTH);
        let scale = gtk::Scale::with_range(gtk::Orientation::Vertical, -12.0, 12.0, 0.5);
        scale.add_css_class("fullscreen-player-equalizer-scale");
        scale.set_inverted(true);
        scale.set_value(0.0);
        scale.set_draw_value(false);
        scale.set_size_request(
            FULLSCREEN_EQUALIZER_MAX_BAND_WIDTH,
            FULLSCREEN_EQUALIZER_MAX_SCALE_HEIGHT,
        );
        scale.set_valign(gtk::Align::Center);
        scale.set_tooltip_text(Some(&equalizer_band_title(index)));
        install_equalizer_scroll(&scale);
        band.append(&scale);
        band.append(&fullscreen_equalizer_band_label(index));
        bands.append(&band);
        band_widgets.push(band);
        scales.push(scale);
    }
    graph.append(&bands);
    let right_levels = fullscreen_equalizer_level_labels(1.0);
    graph.append(&right_levels);
    let left_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    left_spacer.set_hexpand(true);
    let right_spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    right_spacer.set_hexpand(true);
    band_row.append(&left_spacer);
    band_row.append(&graph);
    band_row.append(&right_spacer);
    let band_row_owner = fullscreen_equalizer_allocation_owner(
        &band_row,
        &graph,
        &bands,
        &band_widgets,
        &scales,
        &left_levels,
        &right_levels,
    );
    band_row.set_margin_bottom(0);
    band_row_owner.set_margin_bottom(6);
    content.append(&band_row_owner);
    root.set_child(Some(&content));

    EqualizerPanel {
        root,
        enabled,
        enabled_label,
        scales,
        reset_button,
        preset_label,
        preset_dropdown,
        band_row,
        syncing: Rc::new(Cell::new(false)),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EqualizerFit {
    band_width: i32,
    band_gap: i32,
    scale_height: i32,
    show_right_levels: bool,
}

fn fullscreen_equalizer_fit(row_width: i32, row_height: i32) -> EqualizerFit {
    let fit_inset = fullscreen_equalizer_fit_inset(row_width);
    let row_width = row_width.saturating_sub(fit_inset);
    let show_right_levels = true;
    let side_width = fullscreen_equalizer_total_width(0, 0, show_right_levels);
    let available = row_width.saturating_sub(side_width);
    let band_gaps = EQUALIZER_BAND_COUNT.saturating_sub(1) as i32;
    let full_width = FULLSCREEN_EQUALIZER_MAX_BAND_WIDTH * EQUALIZER_BAND_COUNT as i32
        + FULLSCREEN_EQUALIZER_MAX_BAND_GAP * band_gaps;
    let band_gap = if available >= full_width {
        FULLSCREEN_EQUALIZER_MAX_BAND_GAP
    } else {
        ((available - FULLSCREEN_EQUALIZER_MIN_BAND_WIDTH * EQUALIZER_BAND_COUNT as i32)
            / band_gaps.max(1))
        .clamp(
            FULLSCREEN_EQUALIZER_MIN_BAND_GAP,
            FULLSCREEN_EQUALIZER_MAX_BAND_GAP,
        )
    };
    let band_width = ((available - band_gap * band_gaps) / EQUALIZER_BAND_COUNT as i32).clamp(
        FULLSCREEN_EQUALIZER_MIN_BAND_WIDTH,
        FULLSCREEN_EQUALIZER_MAX_BAND_WIDTH,
    );
    let scale_height = fullscreen_equalizer_scale_height(row_width, row_height, available);

    EqualizerFit {
        band_width,
        band_gap,
        scale_height,
        show_right_levels,
    }
}

fn fullscreen_equalizer_fit_inset(row_width: i32) -> i32 {
    (row_width / 8).clamp(
        FULLSCREEN_EQUALIZER_MIN_FIT_INSET,
        FULLSCREEN_EQUALIZER_MAX_FIT_INSET,
    )
}

fn fullscreen_equalizer_scale_height(row_width: i32, row_height: i32, available: i32) -> i32 {
    let width_budget = row_width.min(available.max(1));
    let width_height = (width_budget * FULLSCREEN_EQUALIZER_MAX_SCALE_HEIGHT / 560).clamp(
        FULLSCREEN_EQUALIZER_MIN_SCALE_HEIGHT,
        FULLSCREEN_EQUALIZER_MAX_SCALE_HEIGHT,
    );
    let height_height = if row_height > 0 {
        row_height
            .saturating_sub(FULLSCREEN_EQUALIZER_LABEL_HEIGHT)
            .clamp(
                FULLSCREEN_EQUALIZER_MIN_SCALE_HEIGHT,
                FULLSCREEN_EQUALIZER_MAX_SCALE_HEIGHT,
            )
    } else {
        FULLSCREEN_EQUALIZER_MAX_SCALE_HEIGHT
    };
    width_height.min(height_height)
}

fn fullscreen_equalizer_total_width(
    band_width: i32,
    band_gap: i32,
    show_right_levels: bool,
) -> i32 {
    let side_width = if show_right_levels {
        FULLSCREEN_EQUALIZER_LEVEL_WIDTH * 2 + FULLSCREEN_EQUALIZER_ROW_GAP * 2
    } else {
        FULLSCREEN_EQUALIZER_LEVEL_WIDTH + FULLSCREEN_EQUALIZER_ROW_GAP
    };
    side_width
        + band_width * EQUALIZER_BAND_COUNT as i32
        + band_gap * EQUALIZER_BAND_COUNT.saturating_sub(1) as i32
}

fn apply_fullscreen_equalizer_fit(
    row_width: i32,
    row_height: i32,
    graph: &gtk::Box,
    bands: &gtk::Box,
    band_widgets: &[gtk::Box],
    scales: &[gtk::Scale],
    levels: (&gtk::Box, &gtk::Box),
) {
    if row_width <= 0 {
        return;
    }

    let fit = fullscreen_equalizer_fit(row_width, row_height);
    let band_area_width = fit.band_width * EQUALIZER_BAND_COUNT as i32
        + fit.band_gap * EQUALIZER_BAND_COUNT.saturating_sub(1) as i32;
    let graph_width =
        fullscreen_equalizer_total_width(fit.band_width, fit.band_gap, fit.show_right_levels);
    graph.set_width_request(graph_width);
    bands.set_spacing(fit.band_gap);
    bands.set_width_request(band_area_width);
    let (left_levels, right_levels) = levels;
    left_levels.set_height_request(fit.scale_height);
    right_levels.set_visible(fit.show_right_levels);
    right_levels.set_height_request(fit.scale_height);
    for (band, scale) in band_widgets.iter().zip(scales.iter()) {
        band.set_width_request(fit.band_width);
        scale.set_size_request(fit.band_width, fit.scale_height);
    }
}

fn fullscreen_equalizer_allocation_owner(
    row: &gtk::Box,
    graph: &gtk::Box,
    bands: &gtk::Box,
    band_widgets: &[gtk::Box],
    scales: &[gtk::Scale],
    left_levels: &gtk::Box,
    right_levels: &gtk::Box,
) -> gtk::Widget {
    let resize_graph = graph.clone();
    let resize_bands = bands.clone();
    let resize_band_widgets = band_widgets.to_vec();
    let resize_scales = scales.to_vec();
    let resize_left_levels = left_levels.clone();
    let resize_right_levels = right_levels.clone();
    let last_allocation = Cell::new(None);
    allocation_owner(row, move |width, height| {
        let allocation = (width, height);
        if width <= 0 || last_allocation.replace(Some(allocation)) == Some(allocation) {
            return;
        }
        apply_fullscreen_equalizer_fit(
            width,
            height,
            &resize_graph,
            &resize_bands,
            &resize_band_widgets,
            &resize_scales,
            (&resize_left_levels, &resize_right_levels),
        );
    })
    .upcast()
}

fn fullscreen_equalizer_level_labels(xalign: f32) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.add_css_class("fullscreen-player-equalizer-levels");
    column.set_width_request(FULLSCREEN_EQUALIZER_LEVEL_WIDTH);
    column.set_height_request(FULLSCREEN_EQUALIZER_MAX_SCALE_HEIGHT);
    column.set_valign(gtk::Align::Start);
    for (index, value) in ["12 dB", "6 dB", "0 dB", "-6 dB", "-12 dB"]
        .iter()
        .enumerate()
    {
        if index > 0 {
            let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
            spacer.set_vexpand(true);
            column.append(&spacer);
        }
        let label = gtk::Label::new(Some(value));
        label.add_css_class("muted");
        label.add_css_class("fullscreen-player-equalizer-level-label");
        label.set_xalign(xalign);
        column.append(&label);
    }
    column
}

fn fullscreen_equalizer_band_label(index: usize) -> gtk::Widget {
    let (value, unit) = equalizer_band_label_parts(index);
    let label = gtk::Box::new(gtk::Orientation::Vertical, 0);
    label.add_css_class("fullscreen-player-equalizer-band-label");
    label.set_halign(gtk::Align::Center);
    for text in [value, unit] {
        let row = gtk::Label::new(Some(&text));
        row.add_css_class("muted");
        row.set_xalign(0.5);
        row.set_width_chars(1);
        row.set_max_width_chars(4);
        row.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.append(&row);
    }
    label.upcast()
}

pub(crate) fn connect_fullscreen_player_controls(shell: &Rc<Shell>) {
    let close_shell = Rc::clone(shell);
    shell
        .player_view
        .fullscreen_player
        .close_button
        .connect_clicked(move |_| close_shell.close_fullscreen_player());
    let close_shell = Rc::clone(shell);
    shell
        .player_view
        .fullscreen_player
        .inline_close_button
        .connect_clicked(move |_| close_shell.close_fullscreen_player());

    let key_shell = Rc::clone(shell);
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape && key_shell.fullscreen_player_visible() {
            key_shell.close_fullscreen_player();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    shell.chrome.window.add_controller(key);

    let queue_tab_shell = Rc::clone(shell);
    shell
        .player_view
        .fullscreen_player
        .stack
        .connect_visible_child_name_notify(move |stack| {
            if !queue_tab_shell.fullscreen_player_visible() {
                return;
            }
            match stack.visible_child_name().as_deref() {
                Some("queue") => queue_tab_shell.schedule_queue_panel_render(),
                Some("lyrics") => {
                    queue_tab_shell.sync_visible_lyrics_surfaces();
                    queue_tab_shell.refresh_fullscreen_lyrics_position();
                }
                Some("equalizer") => queue_tab_shell.refresh_fullscreen_player_layout(),
                _ => {}
            }
            if stack.visible_child_name().as_deref() != Some("lyrics") {
                queue_tab_shell.update_lyrics_highlight();
            }
            queue_tab_shell.sync_visualizer_state();
        });

    let equalizer_shell = Rc::clone(shell);
    let equalizer_guard = Rc::clone(&shell.player_view.fullscreen_player.equalizer_syncing);
    shell
        .player_view
        .fullscreen_player
        .equalizer_enabled
        .connect_state_set(move |row, enabled| {
            if equalizer_guard.get() {
                return glib::Propagation::Proceed;
            }
            row.set_state(enabled);
            equalizer_shell.update_playback_settings(|settings| {
                settings.equalizer.enabled = enabled;
                if settings.equalizer.bands.len() != EQUALIZER_BAND_COUNT {
                    settings.equalizer.sanitize();
                }
            });
            glib::Propagation::Stop
        });

    let pending_equalizer_update = Rc::new(RefCell::new(None::<glib::SourceId>));
    let equalizer_pointer_active = Rc::new(Cell::new(false));
    let equalizer_commit: Rc<dyn Fn()> = {
        let shell = Rc::clone(shell);
        Rc::new(move || shell.update_fullscreen_equalizer_from_scales())
    };
    for scale in &shell.player_view.fullscreen_player.equalizer_scales {
        connect_equalizer_scale_commit(
            scale,
            Rc::clone(&shell.player_view.fullscreen_player.equalizer_syncing),
            Rc::clone(&pending_equalizer_update),
            Rc::clone(&equalizer_pointer_active),
            Rc::clone(&equalizer_commit),
        );
    }

    let reset_shell = Rc::clone(shell);
    shell
        .player_view
        .fullscreen_player
        .equalizer_reset_button
        .connect_clicked(move |_| {
            reset_shell.reset_fullscreen_equalizer_preset();
        });

    let preset_shell = Rc::clone(shell);
    shell
        .player_view
        .fullscreen_player
        .equalizer_preset_dropdown
        .connect_selected_notify(move |dropdown| {
            if preset_shell
                .player_view
                .fullscreen_player
                .equalizer_syncing
                .get()
            {
                return;
            }
            let Some(preset) = equalizer_preset_name_at(dropdown.selected()) else {
                return;
            };
            let bands = equalizer_preset_bands(&preset);
            preset_shell.apply_fullscreen_equalizer_bands(true, preset, bands);
        });
}

fn while_equalizer_syncing(syncing: &Cell<bool>, update: impl FnOnce()) {
    syncing.set(true);
    update();
    syncing.set(false);
}

impl Shell {
    pub(crate) fn open_fullscreen_player(self: &Rc<Self>) {
        let Some(player) = self.selected_playback().as_deref().cloned() else {
            return;
        };
        if player.transport.current.is_none() {
            return;
        }
        self.player_view.fullscreen_player.visible.set(true);
        self.animate_fullscreen_player(true);
        self.update_fullscreen_player_text_fast(&player);
        let text_shell = Rc::clone(self);
        glib::idle_add_local_once(move || {
            if text_shell.fullscreen_player_visible()
                && let Some(player) = text_shell.selected_playback().as_deref()
            {
                text_shell.update_fullscreen_player_text(player);
            }
        });
        self.apply_fullscreen_responsive_layout();
        self.update_fullscreen_player_cover(&player);
        if self
            .player_view
            .fullscreen_player
            .stack
            .visible_child_name()
            .as_deref()
            == Some("queue")
        {
            let queue_shell = Rc::clone(self);
            glib::timeout_add_local_once(
                Duration::from_millis(u64::from(FULLSCREEN_PLAYER_OPEN_TRANSITION_MS)),
                move || {
                    if queue_shell.fullscreen_player_visible()
                        && queue_shell
                            .player_view
                            .fullscreen_player
                            .stack
                            .visible_child_name()
                            .as_deref()
                            == Some("queue")
                    {
                        queue_shell.render_queue_panel();
                    }
                },
            );
        }
        if self
            .player_view
            .fullscreen_player
            .stack
            .visible_child_name()
            .as_deref()
            == Some("lyrics")
        {
            self.sync_visible_lyrics_surfaces();
            let lyrics_shell = Rc::clone(self);
            glib::timeout_add_local_once(
                Duration::from_millis(u64::from(FULLSCREEN_PLAYER_OPEN_TRANSITION_MS)),
                move || {
                    lyrics_shell.refresh_fullscreen_lyrics_position();
                },
            );
        }
        self.sync_visualizer_state();
        let _focused = self.player_view.fullscreen_player.close_button.grab_focus();
    }

    pub(crate) fn close_fullscreen_player(self: &Rc<Self>) {
        if !self.player_view.fullscreen_player.visible.replace(false) {
            return;
        }
        self.animate_fullscreen_player(false);
        self.sync_visualizer_state();
        self.update_lyrics_highlight();
    }

    pub(crate) fn toggle_fullscreen_player(self: &Rc<Self>) {
        if self.fullscreen_player_visible() {
            self.close_fullscreen_player();
        } else {
            self.open_fullscreen_player();
        }
    }

    pub(crate) fn relocalize_fullscreen_player_controls(&self) {
        let selected = {
            let settings = self.settings.current.borrow();
            equalizer_preset_position(&equalizer_selected_preset(&settings.playback.equalizer))
        };
        let fullscreen = &self.player_view.fullscreen_player;
        while_equalizer_syncing(&fullscreen.equalizer_syncing, || {
            relocalize_equalizer_preset_dropdown(&fullscreen.equalizer_preset_dropdown, selected);
        });
    }

    pub(crate) fn update_fullscreen_player(self: &Rc<Self>) {
        if !self.fullscreen_player_visible() {
            return;
        }
        let Some(player) = self.selected_playback().as_deref().cloned() else {
            self.close_fullscreen_player();
            return;
        };
        self.update_fullscreen_player_text(&player);
        self.apply_fullscreen_responsive_layout();
        self.update_fullscreen_player_cover(&player);
        self.sync_fullscreen_equalizer_controls(&self.settings.current.borrow().playback.equalizer);
    }

    pub(crate) fn refresh_fullscreen_player_layout(self: &Rc<Self>) {
        if !self.fullscreen_player_visible() {
            return;
        }
        self.apply_fullscreen_responsive_layout();
    }

    fn apply_fullscreen_responsive_layout(&self) {
        let show_hero = fullscreen_show_hero_for(self.chrome.window.height());
        let compact_equalizer = fullscreen_equalizer_compact_for(
            self.chrome.window.width(),
            self.chrome.window.height(),
        );
        self.player_view
            .fullscreen_player
            .close_button
            .set_visible(show_hero);
        self.player_view
            .fullscreen_player
            .inline_close_button
            .set_visible(!show_hero);
        self.player_view
            .fullscreen_player
            .hero
            .set_visible(show_hero);
        self.player_view
            .fullscreen_player
            .equalizer_band_row
            .set_vexpand(true);
        self.player_view
            .fullscreen_player
            .equalizer_band_row
            .set_height_request(if compact_equalizer {
                FULLSCREEN_EQUALIZER_TINY_ROW_HEIGHT
            } else {
                FULLSCREEN_EQUALIZER_ROW_HEIGHT
            });
        self.player_view
            .fullscreen_player
            .equalizer_band_row
            .set_valign(gtk::Align::Fill);
        let cover_size = self.fullscreen_player_cover_size();
        self.player_view
            .fullscreen_player
            .cover
            .set_square_size(cover_size);
    }

    pub(crate) fn sync_fullscreen_equalizer_controls(&self, equalizer: &EqualizerSettings) {
        self.player_view
            .fullscreen_player
            .equalizer_syncing
            .set(true);
        self.player_view
            .fullscreen_player
            .equalizer_enabled
            .set_active(equalizer.enabled);
        for (index, scale) in self
            .player_view
            .fullscreen_player
            .equalizer_scales
            .iter()
            .enumerate()
        {
            let value = equalizer.bands.get(index).copied().unwrap_or(0.0);
            scale.set_value(value);
        }
        self.sync_fullscreen_equalizer_preset_label(equalizer);
        self.player_view
            .fullscreen_player
            .equalizer_syncing
            .set(false);
    }

    fn sync_fullscreen_equalizer_preset_label(&self, equalizer: &EqualizerSettings) {
        self.player_view
            .fullscreen_player
            .equalizer_preset_dropdown
            .set_selected(equalizer_preset_position(&equalizer_selected_preset(
                equalizer,
            )));
    }

    fn apply_fullscreen_equalizer_bands(
        self: &Rc<Self>,
        enabled: bool,
        preset: String,
        bands: Vec<f64>,
    ) {
        let mut equalizer = self.settings.current.borrow().playback.equalizer.clone();
        equalizer.enabled = enabled;
        equalizer.selected_preset = preset;
        equalizer.bands = bands;
        equalizer.sanitize();
        self.sync_fullscreen_equalizer_controls(&equalizer);
        self.update_playback_settings(|settings| {
            settings.equalizer.enabled = equalizer.enabled;
            settings.equalizer.selected_preset = equalizer.selected_preset;
            settings.equalizer.bands = equalizer.bands;
            settings.equalizer.sanitize();
        });
    }

    fn reset_fullscreen_equalizer_preset(self: &Rc<Self>) {
        let (preset, enabled) = {
            let settings = self.settings.current.borrow();
            (
                equalizer_selected_preset(&settings.playback.equalizer),
                settings.playback.equalizer.enabled,
            )
        };
        let bands = equalizer_default_preset_bands(&preset);
        let mut equalizer = self.settings.current.borrow().playback.equalizer.clone();
        equalizer.enabled = enabled;
        equalizer.selected_preset = preset.clone();
        equalizer.bands = bands.clone();
        equalizer.sanitize();
        self.sync_fullscreen_equalizer_controls(&equalizer);
        self.update_playback_settings(|settings| {
            settings.equalizer.enabled = enabled;
            settings.equalizer.selected_preset = preset;
            settings.equalizer.bands = bands;
            settings.equalizer.sanitize();
        });
    }

    fn update_fullscreen_equalizer_from_scales(self: &Rc<Self>) {
        let bands = self
            .player_view
            .fullscreen_player
            .equalizer_scales
            .iter()
            .map(gtk::Scale::value)
            .collect::<Vec<_>>();
        self.player_view
            .fullscreen_player
            .equalizer_syncing
            .set(true);
        self.player_view
            .fullscreen_player
            .equalizer_preset_dropdown
            .set_selected(equalizer_preset_position("Custom"));
        self.player_view
            .fullscreen_player
            .equalizer_syncing
            .set(false);
        self.update_playback_settings(|settings| {
            if settings.equalizer.bands.len() != EQUALIZER_BAND_COUNT {
                settings.equalizer.sanitize();
            }
            settings.equalizer.bands = bands.clone();
            settings.equalizer.selected_preset = "Custom".to_string();
        });
    }

    fn refresh_fullscreen_lyrics_position(self: &Rc<Self>) {
        if !self.fullscreen_player_visible()
            || self
                .player_view
                .fullscreen_player
                .stack
                .visible_child_name()
                .as_deref()
                != Some("lyrics")
        {
            return;
        }
        self.refocus_fullscreen_lyrics_position();
        let idle_shell = Rc::clone(self);
        glib::idle_add_local_once(move || {
            idle_shell.refocus_fullscreen_lyrics_position();
        });
        let settle_shell = Rc::clone(self);
        glib::timeout_add_local_once(Duration::from_millis(80), move || {
            settle_shell.refocus_fullscreen_lyrics_position();
        });
    }

    fn refocus_fullscreen_lyrics_position(&self) {
        if !self.fullscreen_player_visible()
            || self
                .player_view
                .fullscreen_player
                .stack
                .visible_child_name()
                .as_deref()
                != Some("lyrics")
        {
            return;
        }
        let lyrics = self.visible_lyrics();
        if let Some(selected_lyrics) = self.selected_lyrics() {
            selected_lyrics.fullscreen_pane.refocus_highlight(
                lyrics.as_deref(),
                self.lyrics_position_millis(self.current_position_millis()),
            );
        }
    }

    fn update_fullscreen_player_cover(self: &Rc<Self>, player: &PlaybackView) {
        let cover_size = self.fullscreen_player_cover_size();
        self.player_view
            .fullscreen_player
            .cover
            .set_square_size(cover_size);
        if let Some(entry) = player.transport.current.as_ref() {
            let fetch_size = cover_fetch_size_for_display(cover_size);
            if entry.track.source_id.is_empty() {
                self.clear_fullscreen_player_cover();
                return;
            }
            let source_id = sources::SourceId::new(entry.track.source_id.clone());
            self.bind_playback_artwork_tile(
                &self.player_view.fullscreen_player.cover,
                &source_id,
                entry
                    .track
                    .artwork_binding
                    .as_deref()
                    .map(ArtworkBinding::opaque)
                    .unwrap_or_default(),
                cover_size,
                fetch_size,
            );
        } else {
            self.clear_fullscreen_player_cover();
        }
    }

    fn update_fullscreen_player_text_fast(&self, player: &PlaybackView) {
        self.update_fullscreen_player_labels(player);
    }

    fn update_fullscreen_player_text(&self, player: &PlaybackView) {
        self.update_fullscreen_player_labels(player);
    }

    fn update_fullscreen_player_labels(&self, player: &PlaybackView) {
        let title = player
            .transport
            .current
            .as_ref()
            .map(|entry| entry.track.title.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| tr("Nothing playing"));
        let artist = player
            .transport
            .current
            .as_ref()
            .map(|entry| entry.track.artist.as_str())
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| tr("Queue a track to begin"));
        let album = player
            .transport
            .current
            .as_ref()
            .map(|entry| entry.track.album.as_str())
            .unwrap_or("");
        self.player_view.fullscreen_player.title.set_text(&title);
        self.player_view.fullscreen_player.artist.set_text(&artist);
        self.player_view.fullscreen_player.album.set_text(album);
        self.player_view
            .fullscreen_player
            .title
            .set_sensitive(player.transport.current.is_some());
        self.player_view.fullscreen_player.artist.set_sensitive(
            player
                .transport
                .current
                .as_ref()
                .is_some_and(|entry| !entry.track.artist.is_empty()),
        );
        self.player_view.fullscreen_player.album.set_sensitive(
            player
                .transport
                .current
                .as_ref()
                .is_some_and(|entry| !entry.track.album.is_empty()),
        );
        let parts = fullscreen_player_fast_meta_parts(player.transport.current.as_deref());
        update_fullscreen_meta_row(&self.player_view.fullscreen_player.meta, &parts);
        self.player_view
            .fullscreen_player
            .meta
            .set_visible(!parts.is_empty());
    }

    fn fullscreen_player_cover_size(&self) -> i32 {
        let surface_width = positive_min([
            self.chrome.app_content_stack.width(),
            self.chrome.window.width(),
        ]);
        let allocated_content_width = self.player_view.fullscreen_player.hero_content.width();
        let content_width = if allocated_content_width > 1 {
            allocated_content_width
        } else {
            fullscreen_hero_content_fallback(surface_width)
        };
        let height = positive_min([
            self.chrome.app_content_stack.height(),
            self.chrome
                .window
                .height()
                .saturating_sub(BOTTOM_PLAYER_HEIGHT),
        ]);
        fullscreen_artwork_size_for(content_width, height)
    }

    pub(crate) fn clear_fullscreen_player_cover(self: &Rc<Self>) {
        self.clear_artwork_tile(&self.player_view.fullscreen_player.cover);
    }

    fn animate_fullscreen_player(self: &Rc<Self>, opening: bool) {
        if let Some(tick) = self
            .player_view
            .fullscreen_player
            .animation_tick
            .borrow_mut()
            .take()
        {
            tick.remove();
        }

        let root = self.player_view.fullscreen_player.root.clone();
        let height = self.fullscreen_player_hidden_offset();
        let duration_us = i64::from(if opening {
            FULLSCREEN_PLAYER_OPEN_TRANSITION_MS
        } else {
            FULLSCREEN_PLAYER_CLOSE_TRANSITION_MS
        }) * 1_000;
        let started_at = Rc::new(Cell::new(None));

        root.set_visible(true);
        root.set_opacity(1.0);
        root.set_can_target(opening);
        root.set_sensitive(opening);
        root.set_margin_top(if opening { height } else { 0 });

        let tick_shell = Rc::clone(self);
        let tick_started_at = Rc::clone(&started_at);
        let tick = root.add_tick_callback(move |root, clock| {
            let now = clock.frame_time();
            let start = tick_started_at.get().unwrap_or_else(|| {
                tick_started_at.set(Some(now));
                now
            });
            let elapsed = now.saturating_sub(start);
            let progress = (elapsed as f64 / duration_us as f64).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - progress).powi(3);
            let offset = if opening {
                (1.0 - eased) * f64::from(height)
            } else {
                eased * f64::from(height)
            };
            root.set_margin_top(offset.round() as i32);

            if progress >= 1.0 {
                root.set_can_target(opening);
                root.set_sensitive(opening);
                if opening {
                    root.set_margin_top(0);
                    root.set_opacity(1.0);
                } else {
                    root.set_margin_top(0);
                    root.set_opacity(0.0);
                }
                root.set_visible(opening);
                tick_shell
                    .player_view
                    .fullscreen_player
                    .animation_tick
                    .borrow_mut()
                    .take();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        *self
            .player_view
            .fullscreen_player
            .animation_tick
            .borrow_mut() = Some(tick);
    }

    fn fullscreen_player_hidden_offset(&self) -> i32 {
        self.player_view
            .fullscreen_player
            .root
            .height()
            .max(self.chrome.app_content_stack.height())
            .max((self.chrome.window.height() - BOTTOM_PLAYER_HEIGHT).max(1))
    }
}

fn positive_min(values: impl IntoIterator<Item = i32>) -> i32 {
    values
        .into_iter()
        .filter(|value| *value > 0)
        .min()
        .unwrap_or(1)
}

fn fullscreen_player_label(css_class: &str) -> gtk::Label {
    let label = gtk::Label::new(None);
    label.add_css_class(css_class);
    label.set_xalign(0.5);
    label.set_justify(gtk::Justification::Center);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_lines(2);
    label.set_ellipsize(gtk::pango::EllipsizeMode::None);
    label.set_width_chars(1);
    label.set_max_width_chars(48);
    label.set_width_request(1);
    label.set_halign(gtk::Align::Fill);
    label
}

fn fullscreen_player_meta_row() -> gtk::FlowBox {
    let row = gtk::FlowBox::new();
    row.add_css_class("fullscreen-player-meta-row");
    row.set_column_spacing(4);
    row.set_row_spacing(4);
    row.set_selection_mode(gtk::SelectionMode::None);
    row.set_max_children_per_line(4);
    row.set_halign(gtk::Align::Start);
    row.set_valign(gtk::Align::Center);
    row
}

fn update_fullscreen_meta_row(row: &gtk::FlowBox, parts: &[String]) {
    while let Some(child) = row.first_child() {
        row.remove(&child);
    }

    for part in parts {
        let label = gtk::Label::new(Some(part));
        label.add_css_class("album-detail-genre-pill");
        label.add_css_class("fullscreen-player-meta-pill");
        label.set_xalign(0.5);
        row.insert(&label, -1);
    }
}

fn fullscreen_player_entry_meta_parts(
    entry: Option<&CurrentMedia>,
    source_label: Option<&str>,
) -> Vec<String> {
    let Some(entry) = entry else {
        return Vec::new();
    };
    fullscreen_player_meta_parts(entry.track.year, source_label)
}

fn fullscreen_player_fast_meta_parts(entry: Option<&CurrentMedia>) -> Vec<String> {
    let source_label = entry.and_then(queue_entry_source_label);
    fullscreen_player_entry_meta_parts(entry, source_label.as_deref())
}

fn queue_entry_source_label(entry: &CurrentMedia) -> Option<String> {
    entry
        .track
        .source_format
        .as_deref()
        .and_then(audio_source_label_from_format)
        .or_else(|| {
            entry
                .track
                .media_uri
                .as_deref()
                .and_then(audio_source_label_from_path)
        })
}

fn fullscreen_player_meta_parts(year: Option<i64>, source_label: Option<&str>) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(source) = source_label
        .map(str::trim)
        .filter(|source| !source.is_empty())
    {
        parts.push(source.to_string());
    }
    if let Some(year) = year.filter(|year| *year > 0) {
        parts.push(year.to_string());
    }
    parts
}

fn audio_source_label_from_path(path: &str) -> Option<String> {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let extension = Path::new(path).extension()?.to_str()?.trim();
    audio_source_label_from_format(extension)
}

fn audio_source_label_from_format(value: &str) -> Option<String> {
    let value = value
        .rsplit('/')
        .next()
        .unwrap_or(value)
        .trim()
        .trim_start_matches('.');
    if value.is_empty() {
        return None;
    }
    let normalized = match value.to_ascii_lowercase().as_str() {
        "mpeg" | "mpga" => "MP3".to_string(),
        other => other.to_ascii_uppercase(),
    };
    Some(normalized)
}

fn fullscreen_hero_content_fallback(surface_width: i32) -> i32 {
    surface_width
        .saturating_sub(FULLSCREEN_PLAYER_FALLBACK_HORIZONTAL_RESERVED)
        .max(1)
}

fn fullscreen_artwork_size_for(content_width: i32, height: i32) -> i32 {
    let width_limit = content_width
        .saturating_sub(FULLSCREEN_PLAYER_HERO_SPACING)
        .max(1)
        / 2;
    let height_limit = (height - FULLSCREEN_PLAYER_VERTICAL_RESERVED).max(1);
    width_limit.min(height_limit).clamp(
        FULLSCREEN_PLAYER_MIN_COVER_SIZE,
        FULLSCREEN_PLAYER_MAX_COVER_SIZE,
    )
}

fn fullscreen_show_hero_for(height: i32) -> bool {
    height >= FULLSCREEN_PLAYER_HERO_MIN_WINDOW_HEIGHT
}

fn fullscreen_equalizer_compact_for(width: i32, height: i32) -> bool {
    width < MIN_USEFUL_MAIN_WIDTH || !fullscreen_show_hero_for(height)
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn fullscreen_artwork_shares_narrow_hero_width_with_metadata() {
        assert_eq!(fullscreen_hero_content_fallback(450), 264);
        assert_eq!(
            fullscreen_artwork_size_for(fullscreen_hero_content_fallback(450), 900),
            FULLSCREEN_PLAYER_MIN_COVER_SIZE
        );
        assert_eq!(
            fullscreen_artwork_size_for(fullscreen_hero_content_fallback(900), 700),
            270
        );
        assert_eq!(
            fullscreen_artwork_size_for(fullscreen_hero_content_fallback(1_440), 900),
            FULLSCREEN_PLAYER_MAX_COVER_SIZE
        );
    }
}

#[cfg(test)]
mod playback_refresh_tests {
    use super::*;
    use playback::{
        ControlsView, CurrentMediaId, OccurrenceId, PlaybackMedia, PlaybackOutput, Provenance,
        QueueSummaryView, RepeatMode, RunId, SourceSessionEpoch, TransportStatus, TransportView,
    };
    use std::sync::Arc;

    #[test]
    fn fullscreen_refresh_ignores_position_ticks_but_replaces_current_media() {
        let source = library::SourceKey::from_raw(1);
        let previous = playback_view(source, Some(current_media("Current", source)), 1_000);

        let mut position_tick = previous.clone();
        position_tick.transport.position_millis = 1_500;
        assert_eq!(
            fullscreen_playback_refresh(Some(&previous), &position_tick),
            FullscreenPlaybackRefresh::None
        );

        let mut state_change = previous.clone();
        state_change.transport.state = TransportStatus::Paused;
        state_change.transport.desired_playing = false;
        assert_eq!(
            fullscreen_playback_refresh(Some(&previous), &state_change),
            FullscreenPlaybackRefresh::Visualizer
        );

        let mut current_change = previous.clone();
        current_change.transport.current = Some(Arc::new(current_media("Next", source)));
        assert_eq!(
            fullscreen_playback_refresh(Some(&previous), &current_change),
            FullscreenPlaybackRefresh::Static
        );
    }

    fn current_media(title: &str, source: library::SourceKey) -> CurrentMedia {
        CurrentMedia {
            id: CurrentMediaId {
                source_key: source,
                source_session_epoch: SourceSessionEpoch::new(1),
                run: Some(RunId::new(1)),
                occurrence: OccurrenceId::new(format!("queue:{title}")),
            },
            track: PlaybackMedia {
                source_id: "source".to_string(),
                track_key: None,
                track_object_id: title.to_string(),
                title: title.to_string(),
                artist: "Artist".to_string(),
                album: "Album".to_string(),
                album_display_artist: None,
                album_key: None,
                primary_artist_key: None,
                media_uri: None,
                artwork_binding: None,
                duration_millis: 180_000,
                disc_number: None,
                track_number: None,
                year: None,
                release_date: None,
                favorite: None,
                rating: None,
                is_downloaded: false,
                source_format: None,
                musicbrainz_recording_id: None,
                musicbrainz_release_track_id: None,
                musicbrainz_album_id: None,
                musicbrainz_release_group_id: None,
                primary_artist_musicbrainz_id: None,
                cue_path: None,
                cue_start_millis: None,
                cue_end_millis: None,
                artist_links: Vec::new(),
            },
            provenance: Provenance::Manual,
        }
    }

    fn playback_view(
        source: library::SourceKey,
        current: Option<CurrentMedia>,
        position_millis: u64,
    ) -> PlaybackView {
        let current_occurrence = current.as_ref().map(|media| media.id.occurrence.clone());
        PlaybackView {
            queue: QueueSummaryView {
                revision: 1,
                total: usize::from(current.is_some()),
                current_occurrence,
                current_index: current.as_ref().map(|_| 0),
                current_position: current.as_ref().map(|_| 0),
                next_occurrence: None,
            },
            transport: TransportView {
                source_id: source,
                current: current.map(Arc::new),
                state: TransportStatus::Playing,
                desired_playing: true,
                position_millis,
                duration_millis: 180_000,
                can_seek: true,
                buffering_percent: None,
                error: None,
            },
            controls: ControlsView {
                repeat_mode: RepeatMode::Off,
                shuffle_enabled: false,
                auto_dj_enabled: false,
                volume: 1.0,
                muted: false,
                audio_output: None,
                playback_output: PlaybackOutput::Local,
            },
        }
    }
}
