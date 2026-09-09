use super::{equalizer::EqualizerSurface, icons::lyrics_icon_area};
use adw::prelude::*;
use localization::tr;
use playback::{EqualizerSettings, PlaybackView};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use ui_shared::artwork::ArtworkTile;
pub const FULLSCREEN_PLAYER_OPEN_TRANSITION_MS: u32 = 420;
pub const FULLSCREEN_PLAYER_CLOSE_TRANSITION_MS: u32 = 320;
pub const FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE: i32 = 320;
pub const FULLSCREEN_PLAYER_MIN_COVER_SIZE: i32 = 140;
pub const FULLSCREEN_PLAYER_MAX_COVER_SIZE: i32 = 320;
pub const FULLSCREEN_PLAYER_FALLBACK_HORIZONTAL_RESERVED: i32 = 186;
pub const FULLSCREEN_PLAYER_HERO_SPACING: i32 = 18;
pub const FULLSCREEN_PLAYER_PANE_RESERVED_HEIGHT: i32 = 260;
pub const FULLSCREEN_PLAYER_TINY_COVER_SIZE: i32 = 64;
pub const FULLSCREEN_PLAYER_MIN_SIDE_DETAILS_WIDTH: i32 = 120;
pub const FULLSCREEN_PLAYER_HERO_LINE_SPACING: i32 = 12;
pub const FULLSCREEN_ICON_SIZE: i32 = 18;
pub struct FullscreenPlayerParts {
    pub root: gtk::Overlay,
    pub visible: Cell<bool>,
    pub animation_tick: RefCell<Option<gtk::TickCallbackId>>,
    pub slide_offset: Rc<Cell<i32>>,
    pub close_button: gtk::Button,
    pub inline_close_button: gtk::Button,
    pub hero: gtk::Box,
    pub hero_content: adw::WrapBox,
    pub details: gtk::Box,
    pub cover: ArtworkTile,
    pub cover_size: Cell<i32>,
    pub title: gtk::Label,
    pub artist: gtk::Label,
    pub album: gtk::Label,
    pub meta: gtk::FlowBox,
    pub stack: adw::ViewStack,
    pub tabs: Vec<(gtk::ToggleButton, gtk::Label)>,
    pub switcher: gtk::Box,
    pub inline_start: gtk::Box,
    pub inline_end: gtk::Box,
    pub tabs_labeled_width: Rc<Cell<i32>>,
    pub lyrics_host: gtk::Box,
    pub queue_header: gtk::Widget,
    pub queue_panel: gtk::Box,
    pub queue_loading: adw::Spinner,
    pub equalizer: EqualizerSurface,
}

impl FullscreenPlayerParts {
    pub fn set_slide_offset(&self, offset: i32) {
        if self.slide_offset.replace(offset) != offset
            && let Some(parent) = self.root.parent()
        {
            parent.queue_allocate();
        }
    }
}

pub fn build_fullscreen_player(
    hero_start_controls: &impl IsA<gtk::Widget>,
    hero_end_controls: &impl IsA<gtk::Widget>,
    inline_start_controls: &impl IsA<gtk::Widget>,
    inline_end_controls: &impl IsA<gtk::Widget>,
    visualizer_area: &gtk::DrawingArea,
    drag_handle: &gtk::WindowHandle,
) -> FullscreenPlayerParts {
    let resource = crate::ui_resource::FULLSCREEN_PLAYER_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        root: gtk::Overlay,
        close_button: gtk::Button,
        inline_close_button: gtk::Button,
        hero: gtk::Box,
        hero_content: adw::WrapBox,
        details: gtk::Box,
        title: gtk::Label,
        artist: gtk::Label,
        album: gtk::Label,
        meta: gtk::FlowBox,
        stack: adw::ViewStack,
        switcher_bar: gtk::Overlay,
        switcher: gtk::Box,
        queue_tab: gtk::ToggleButton,
        queue_tab_label: gtk::Label,
        lyrics_tab: gtk::ToggleButton,
        lyrics_tab_icon: gtk::Box,
        lyrics_tab_label: gtk::Label,
        visualizer_tab: gtk::ToggleButton,
        visualizer_tab_icon: gtk::Box,
        visualizer_tab_label: gtk::Label,
        equalizer_tab: gtk::ToggleButton,
        equalizer_tab_icon: gtk::Box,
        equalizer_tab_label: gtk::Label,
        inline_start: gtk::Box,
        inline_end: gtk::Box,
        queue_panel: gtk::Box,
        queue_loading: adw::Spinner,
        fullscreen_queue_header: gtk::Box,
        fullscreen_queue_album: gtk::Label,
        fullscreen_queue_year: gtk::Label,
        lyrics_host: gtk::Box,
        visualizer_panel: gtk::Box,
        equalizer_panel: gtk::ScrolledWindow,
    });
    hero.set_spacing(FULLSCREEN_PLAYER_HERO_SPACING);
    hero_content.set_child_spacing(FULLSCREEN_PLAYER_HERO_SPACING);
    hero.append(hero_start_controls);
    hero.append(&close_button);

    let cover = ArtworkTile::new(FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE);
    cover.area.add_css_class("fullscreen-player-cover");
    cover.area.set_halign(gtk::Align::End);
    cover.area.set_valign(gtk::Align::Center);
    hero_content.prepend(&cover.area);
    hero.append(&hero_content);
    hero.append(hero_end_controls);
    let queue_header = super::queue::fullscreen_queue_column_owner(
        &fullscreen_queue_header,
        super::queue::QueueFullscreenColumnWidgets {
            album: fullscreen_queue_album.upcast(),
            year: fullscreen_queue_year.upcast(),
        },
    );
    queue_header.set_visible(false);
    queue_panel.prepend(&queue_header);
    stack.add_titled(&queue_panel, Some("queue"), &tr("Queue"));
    stack.add_titled(&lyrics_host, Some("lyrics"), &tr("Lyrics"));
    visualizer_panel.append(visualizer_area);
    stack.add_titled(&visualizer_panel, Some("visualizer"), &tr("Visualizer"));

    let equalizer = EqualizerSurface::new(&EqualizerSettings::default());
    equalizer_panel.set_child(Some(&equalizer.root));
    stack.add_titled(&equalizer_panel, Some("equalizer"), &tr("Equalizer"));
    stack.set_visible_child_name("lyrics");

    inline_start.append(inline_start_controls);
    inline_start.append(&inline_close_button);
    inline_end.append(inline_end_controls);
    lyrics_tab_icon.append(&lyrics_icon_area(Rc::new(Cell::new(true))));
    visualizer_tab_icon.append(&fullscreen_visualizer_icon());
    equalizer_tab_icon.append(&fullscreen_equalizer_icon());
    let tabs = connect_fullscreen_player_switcher(
        &stack,
        [
            (queue_tab, queue_tab_label, "queue"),
            (lyrics_tab, lyrics_tab_label, "lyrics"),
            (visualizer_tab, visualizer_tab_label, "visualizer"),
            (equalizer_tab, equalizer_tab_label, "equalizer"),
        ],
    );
    switcher_bar.set_measure_overlay(&inline_start, false);
    switcher_bar.set_measure_overlay(&inline_end, false);
    let tabs_labeled_width = Rc::new(Cell::new(0));
    root.add_overlay(drag_handle);
    root.set_measure_overlay(drag_handle, false);

    FullscreenPlayerParts {
        root,
        visible: Cell::new(false),
        animation_tick: RefCell::new(None),
        slide_offset: Rc::new(Cell::new(0)),
        close_button,
        inline_close_button,
        hero,
        hero_content,
        details,
        cover,
        cover_size: Cell::new(FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE),
        title,
        artist,
        album,
        meta,
        stack,
        tabs,
        switcher,
        inline_start,
        inline_end,
        tabs_labeled_width,
        lyrics_host,
        queue_header,
        queue_panel,
        queue_loading,
        equalizer,
    }
}

pub fn connect_fullscreen_player_switcher(
    stack: &adw::ViewStack,
    tabs: [(gtk::ToggleButton, gtk::Label, &'static str); 4],
) -> Vec<(gtk::ToggleButton, gtk::Label)> {
    let mut selection = Vec::with_capacity(4);
    let mut labels = Vec::with_capacity(4);
    for (button, label, page) in tabs {
        let page_stack = stack.downgrade();
        button.connect_clicked(move |_| {
            if let Some(stack) = page_stack.upgrade() {
                stack.set_visible_child_name(page);
            }
        });
        selection.push((button.downgrade(), page));
        labels.push((button, label));
    }
    stack.connect_visible_child_name_notify(move |stack| {
        let page = stack.visible_child_name();
        for (button, name) in &selection {
            if let Some(button) = button.upgrade() {
                button.set_active(page.as_deref() == Some(*name));
            }
        }
    });
    labels
}

pub fn fullscreen_visualizer_icon() -> gtk::DrawingArea {
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

use crate::{bottom::BOTTOM_PLAYER_HEIGHT, state::NowPlayingPresentation};
use gtk::glib;
use std::time::Duration;
use ui_shared::artwork::cover_fetch_size_for_display;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FullscreenPlaybackRefresh {
    None,
    Visualizer,
    Static,
}

pub fn fullscreen_playback_refresh(
    previous: Option<&PlaybackView>,
    next: &PlaybackView,
) -> FullscreenPlaybackRefresh {
    let Some(previous) = previous else {
        return FullscreenPlaybackRefresh::Static;
    };
    if previous.transport.current != next.transport.current {
        FullscreenPlaybackRefresh::Static
    } else if previous.transport.effective_state() != next.transport.effective_state() {
        FullscreenPlaybackRefresh::Visualizer
    } else {
        FullscreenPlaybackRefresh::None
    }
}

pub fn connect_fullscreen_player_controls(shell: &Rc<crate::PlayerUi>) {
    let close_shell = Rc::clone(shell);
    shell
        .views
        .fullscreen_player
        .close_button
        .connect_clicked(move |_| close_shell.close_fullscreen_player());
    let close_shell = Rc::clone(shell);
    shell
        .views
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
    if let Some(window) = shell.window.upgrade() {
        window.add_controller(key);
    }

    let queue_tab_shell = Rc::clone(shell);
    shell
        .views
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
    shell
        .views
        .fullscreen_player
        .equalizer
        .connect_changed(move |equalizer| {
            equalizer_shell.update_playback_settings(|settings| {
                settings.equalizer = equalizer.clone();
            });
        });
}

impl crate::PlayerUi {
    pub fn open_fullscreen_player(self: &Rc<Self>) {
        let Some(player) = self.selected_playback().as_deref().cloned() else {
            return;
        };
        if player.transport.current.is_none() {
            return;
        }
        self.views.fullscreen_player.visible.set(true);
        self.animate_fullscreen_player(true);
        let presentation = NowPlayingPresentation::new(Some(&player));
        self.apply_fullscreen_now_playing_text(&presentation);
        self.apply_fullscreen_responsive_layout();
        self.apply_fullscreen_now_playing_cover(&presentation);
        if self
            .views
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
                            .views
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
            .views
            .fullscreen_player
            .stack
            .visible_child_name()
            .as_deref()
            == Some("lyrics")
        {
            self.sync_visible_lyrics_surfaces();
        }
        self.sync_visualizer_state();
        let _focused = self.views.fullscreen_player.close_button.grab_focus();
    }

    pub fn close_fullscreen_player(self: &Rc<Self>) {
        if !self.views.fullscreen_player.visible.replace(false) {
            return;
        }
        self.animate_fullscreen_player(false);
        self.sync_visualizer_state();
        self.sync_visible_lyrics_surfaces();
    }

    pub fn toggle_fullscreen_player(self: &Rc<Self>) {
        if self.fullscreen_player_visible() {
            self.close_fullscreen_player();
        } else {
            self.open_fullscreen_player();
        }
    }

    pub fn update_fullscreen_player_with(self: &Rc<Self>, presentation: &NowPlayingPresentation) {
        if !self.fullscreen_player_visible() {
            return;
        }
        self.apply_fullscreen_now_playing_text(presentation);
        self.apply_fullscreen_responsive_layout();
        self.apply_fullscreen_now_playing_cover(presentation);
        self.sync_fullscreen_equalizer_controls(&self.settings.current.borrow().playback.equalizer);
    }

    pub fn refresh_fullscreen_player_layout(self: &Rc<Self>) {
        if !self.fullscreen_player_visible() {
            return;
        }
        self.apply_fullscreen_responsive_layout();
    }

    fn apply_fullscreen_responsive_layout(&self) {
        self.apply_fullscreen_responsive_layout_for_size(
            self.window.upgrade().map_or(0, |window| window.width()),
            self.window.upgrade().map_or(0, |window| window.height()),
        );
    }

    pub fn apply_fullscreen_responsive_layout_for_size(&self, width: i32, height: i32) {
        let fullscreen = &self.views.fullscreen_player;
        let hero_content_width = fullscreen_hero_content_fallback(width);
        let hero_capacity = fullscreen_hero_capacity(height);
        let normal_cover_size = fullscreen_artwork_size_for(hero_content_width);
        let details = &fullscreen.details;
        details.set_width_request(1);
        details.set_height_request(-1);
        self.apply_fullscreen_details_alignment(false);
        let side_cover_size = normal_cover_size.min(hero_capacity.max(1));
        let side_details_width = hero_content_width
            .saturating_sub(side_cover_size)
            .saturating_sub(FULLSCREEN_PLAYER_HERO_SPACING)
            .max(1);
        let side_text_exceeds_line_budget =
            fullscreen_label_exceeds_lines(&fullscreen.title, side_details_width, 4)
                || fullscreen_label_exceeds_lines(&fullscreen.artist, side_details_width, 2)
                || fullscreen_label_exceeds_lines(&fullscreen.album, side_details_width, 2);
        let (_, side_details_height, _, _) =
            details.measure(gtk::Orientation::Vertical, side_details_width);
        let details_below = fullscreen_details_below_cover(
            side_details_width,
            side_details_height,
            side_cover_size,
            side_text_exceeds_line_budget,
        );
        let window_allows_hero = fullscreen_window_allows_hero(width, height);
        let (cover_size, hero_fits) = if details_below {
            details.set_width_request(hero_content_width);
            self.apply_fullscreen_details_alignment(true);
            fullscreen
                .hero_content
                .set_wrap_policy(adw::WrapPolicy::Natural);
            let (_, details_height, _, _) =
                details.measure(gtk::Orientation::Vertical, hero_content_width);
            match fullscreen_below_cover_size(hero_capacity, details_height, normal_cover_size) {
                Some(cover_size) => (cover_size, true),
                None => (FULLSCREEN_PLAYER_TINY_COVER_SIZE, false),
            }
        } else {
            fullscreen
                .hero_content
                .set_wrap_policy(adw::WrapPolicy::Minimum);
            (
                side_cover_size,
                side_cover_size >= FULLSCREEN_PLAYER_TINY_COVER_SIZE
                    && side_cover_size.max(side_details_height) <= hero_capacity,
            )
        };
        let show_hero = window_allows_hero && hero_fits;
        fullscreen.close_button.set_visible(show_hero);
        fullscreen.inline_close_button.set_visible(!show_hero);
        fullscreen.hero.set_visible(show_hero);
        fullscreen
            .cover
            .set_square_size(cover_size.max(FULLSCREEN_PLAYER_TINY_COVER_SIZE));
        fullscreen
            .cover_size
            .set(cover_size.max(FULLSCREEN_PLAYER_TINY_COVER_SIZE));

        let labeled_width = fullscreen_labeled_switcher_natural_width(fullscreen);
        let (start_width, end_width) = if show_hero {
            (0, 0)
        } else {
            let (_, start_width, _, _) = fullscreen
                .inline_start
                .measure(gtk::Orientation::Horizontal, -1);
            let (_, end_width, _, _) = fullscreen
                .inline_end
                .measure(gtk::Orientation::Horizontal, -1);
            (start_width, end_width)
        };
        let show_tab_labels =
            !fullscreen_tabs_compact_for(show_hero, width, labeled_width, start_width, end_width);
        for (_, label) in &fullscreen.tabs {
            label.set_visible(show_tab_labels);
        }
    }

    fn apply_fullscreen_details_alignment(&self, centered: bool) {
        let align = if centered {
            gtk::Align::Fill
        } else {
            gtk::Align::Start
        };
        let xalign = if centered { 0.5 } else { 0.0 };
        let justification = if centered {
            gtk::Justification::Center
        } else {
            gtk::Justification::Left
        };
        for label in [
            &self.views.fullscreen_player.title,
            &self.views.fullscreen_player.artist,
            &self.views.fullscreen_player.album,
        ] {
            label.set_halign(align);
            label.set_xalign(xalign);
            label.set_justify(justification);
        }
        self.views.fullscreen_player.meta.set_halign(if centered {
            gtk::Align::Center
        } else {
            gtk::Align::Start
        });
    }

    fn apply_fullscreen_now_playing_cover(self: &Rc<Self>, presentation: &NowPlayingPresentation) {
        let cover_size = self.fullscreen_player_cover_size();
        self.views
            .fullscreen_player
            .cover
            .set_square_size(cover_size);
        if presentation.current {
            let fetch_size = cover_fetch_size_for_display(cover_size);
            self.artwork.bind_playback_artwork_tile(
                &self.views.fullscreen_player.cover,
                presentation.artwork.clone(),
                cover_size,
                fetch_size,
            );
        } else {
            self.clear_fullscreen_player_cover();
        }
    }

    fn apply_fullscreen_now_playing_text(&self, presentation: &NowPlayingPresentation) {
        self.views
            .fullscreen_player
            .title
            .set_text(&presentation.title);
        self.views
            .fullscreen_player
            .artist
            .set_text(&presentation.artist);
        self.views
            .fullscreen_player
            .album
            .set_text(&presentation.album);
        self.views
            .fullscreen_player
            .title
            .set_sensitive(presentation.current);
        self.views
            .fullscreen_player
            .artist
            .set_sensitive(presentation.current && !presentation.artist.is_empty());
        self.views
            .fullscreen_player
            .album
            .set_sensitive(presentation.current && !presentation.album.is_empty());
        update_fullscreen_meta_row(&self.views.fullscreen_player.meta, &presentation.meta);
        self.views
            .fullscreen_player
            .meta
            .set_visible(!presentation.meta.is_empty());
    }

    fn fullscreen_player_cover_size(&self) -> i32 {
        self.views.fullscreen_player.cover_size.get()
    }

    pub fn clear_fullscreen_player_cover(self: &Rc<Self>) {
        self.artwork
            .clear_artwork_tile(&self.views.fullscreen_player.cover);
    }

    fn refresh_fullscreen_lyrics_position(self: &Rc<Self>) {
        if !self.fullscreen_player_visible()
            || self
                .views
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
                .views
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

    fn animate_fullscreen_player(self: &Rc<Self>, opening: bool) {
        if let Some(tick) = self
            .views
            .fullscreen_player
            .animation_tick
            .borrow_mut()
            .take()
        {
            tick.remove();
        }

        let root = self.views.fullscreen_player.root.clone();
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
        self.views
            .fullscreen_player
            .set_slide_offset(if opening { height } else { 0 });

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
            tick_shell
                .views
                .fullscreen_player
                .set_slide_offset(offset.round() as i32);

            if progress >= 1.0 {
                root.set_can_target(opening);
                root.set_sensitive(opening);
                if opening {
                    tick_shell.views.fullscreen_player.set_slide_offset(0);
                    root.set_opacity(1.0);
                } else {
                    tick_shell.views.fullscreen_player.set_slide_offset(0);
                    root.set_opacity(0.0);
                }
                root.set_visible(opening);
                tick_shell
                    .views
                    .fullscreen_player
                    .animation_tick
                    .borrow_mut()
                    .take();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        *self.views.fullscreen_player.animation_tick.borrow_mut() = Some(tick);
    }

    fn fullscreen_player_hidden_offset(&self) -> i32 {
        self.views
            .fullscreen_player
            .root
            .height()
            .max(
                self.content_stack
                    .upgrade()
                    .map_or(0, |stack| stack.height()),
            )
            .max(
                (self.window.upgrade().map_or(0, |window| window.height()) - BOTTOM_PLAYER_HEIGHT)
                    .max(1),
            )
    }
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

fn fullscreen_hero_content_fallback(surface_width: i32) -> i32 {
    surface_width
        .saturating_sub(FULLSCREEN_PLAYER_FALLBACK_HORIZONTAL_RESERVED)
        .max(1)
}

fn fullscreen_artwork_size_for(content_width: i32) -> i32 {
    let width_limit = content_width
        .saturating_sub(FULLSCREEN_PLAYER_HERO_SPACING)
        .max(1)
        / 2;
    width_limit.clamp(
        FULLSCREEN_PLAYER_MIN_COVER_SIZE,
        FULLSCREEN_PLAYER_MAX_COVER_SIZE,
    )
}

fn fullscreen_details_below_cover(
    side_details_width: i32,
    side_details_height: i32,
    cover_size: i32,
    text_exceeds_line_budget: bool,
) -> bool {
    side_details_width < FULLSCREEN_PLAYER_MIN_SIDE_DETAILS_WIDTH
        || side_details_height > cover_size
        || text_exceeds_line_budget
}

fn fullscreen_label_exceeds_lines(label: &gtk::Label, width: i32, maximum_lines: i32) -> bool {
    let layout = label.layout().copy();
    layout.set_width(width.max(1) * gtk::pango::SCALE);
    layout.set_height(-1);
    layout.set_wrap(gtk::pango::WrapMode::WordChar);
    layout.set_ellipsize(gtk::pango::EllipsizeMode::None);
    layout.line_count() > maximum_lines
}

fn fullscreen_below_cover_size(
    hero_height: i32,
    details_height: i32,
    normal_cover_size: i32,
) -> Option<i32> {
    let available_cover_height = hero_height
        .saturating_sub(FULLSCREEN_PLAYER_HERO_LINE_SPACING)
        .saturating_sub(details_height);
    (available_cover_height >= FULLSCREEN_PLAYER_TINY_COVER_SIZE)
        .then(|| normal_cover_size.min(available_cover_height))
}

fn fullscreen_hero_capacity(height: i32) -> i32 {
    height
        .saturating_sub(BOTTOM_PLAYER_HEIGHT)
        .saturating_sub(FULLSCREEN_PLAYER_PANE_RESERVED_HEIGHT)
        .clamp(0, 560)
}

fn fullscreen_window_allows_hero(width: i32, height: i32) -> bool {
    let minimum_content_width = FULLSCREEN_PLAYER_TINY_COVER_SIZE
        + FULLSCREEN_PLAYER_HERO_SPACING
        + FULLSCREEN_PLAYER_MIN_SIDE_DETAILS_WIDTH;
    fullscreen_hero_capacity(height) >= FULLSCREEN_PLAYER_TINY_COVER_SIZE
        && fullscreen_hero_content_fallback(width) >= minimum_content_width
}

fn fullscreen_labeled_switcher_natural_width(fullscreen: &FullscreenPlayerParts) -> i32 {
    let cached = fullscreen.tabs_labeled_width.get();
    if cached > 0 {
        return cached;
    }
    for (_, label) in &fullscreen.tabs {
        label.set_visible(true);
    }
    let (_, natural, _, _) = fullscreen
        .switcher
        .measure(gtk::Orientation::Horizontal, -1);
    fullscreen.tabs_labeled_width.set(natural);
    natural
}

fn fullscreen_tabs_compact_for(
    show_hero: bool,
    width: i32,
    labeled_width: i32,
    start_width: i32,
    end_width: i32,
) -> bool {
    labeled_width > width
        || (!show_hero
            && width < labeled_width.saturating_add(start_width.max(end_width).saturating_mul(2)))
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn fullscreen_artwork_shares_narrow_hero_width_with_metadata() {
        assert_eq!(fullscreen_hero_content_fallback(450), 264);
        assert_eq!(
            fullscreen_artwork_size_for(fullscreen_hero_content_fallback(450)),
            FULLSCREEN_PLAYER_MIN_COVER_SIZE
        );
        assert_eq!(
            fullscreen_artwork_size_for(fullscreen_hero_content_fallback(900)),
            FULLSCREEN_PLAYER_MAX_COVER_SIZE
        );
        assert_eq!(
            fullscreen_artwork_size_for(fullscreen_hero_content_fallback(1_440)),
            FULLSCREEN_PLAYER_MAX_COVER_SIZE
        );
    }

    #[test]
    fn fullscreen_hero_uses_side_and_below_presentations() {
        assert!(!fullscreen_details_below_cover(300, 100, 140, false));
        assert!(fullscreen_details_below_cover(100, 80, 140, false));
        assert!(fullscreen_details_below_cover(300, 100, 140, true));
        assert!(fullscreen_details_below_cover(300, 141, 140, false));
        assert_eq!(fullscreen_below_cover_size(560, 220, 320), Some(320));
        assert_eq!(fullscreen_below_cover_size(400, 220, 320), Some(168));
        assert_eq!(fullscreen_below_cover_size(280, 220, 320), None);
        assert_eq!(fullscreen_hero_capacity(679), 322);
        assert_eq!(fullscreen_hero_capacity(900), 543);
    }

    #[test]
    fn fullscreen_hero_visibility_depends_only_on_window_space() {
        assert!(fullscreen_window_allows_hero(388, 421));
        assert!(!fullscreen_window_allows_hero(387, 421));
        assert!(!fullscreen_window_allows_hero(900, 420));
    }

    #[test]
    fn fullscreen_tabs_compact_when_labels_exceed_the_window() {
        assert!(fullscreen_tabs_compact_for(true, 339, 340, 50, 60));
        assert!(!fullscreen_tabs_compact_for(true, 340, 340, 50, 60));
    }

    #[test]
    fn fullscreen_tabs_add_side_clearance_on_the_shared_top_row() {
        assert!(!fullscreen_tabs_compact_for(false, 460, 340, 50, 60));
        assert!(fullscreen_tabs_compact_for(false, 459, 340, 50, 60));
    }
}

#[cfg(test)]
mod playback_refresh_tests {
    use super::*;
    use playback::{
        ControlsView, CurrentMedia, CurrentMediaId, OccurrenceId, PlaybackOutput, Provenance,
        QueueItem, QueueOccurrence, QueueSummaryView, RepeatMode, RunId, TransportStatus,
        TransportView,
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
        let presentation = NowPlayingPresentation::new(Some(&current_change));
        assert!(presentation.current);
        assert_eq!(presentation.title, "Next");
        assert_eq!(presentation.artist, "Artist");
        assert_eq!(presentation.album, "Album");
        assert_eq!(presentation.meta, ["MP3", "2026"]);
    }

    fn current_media(title: &str, _source: library::SourceKey) -> CurrentMedia {
        let occurrence = OccurrenceId::new(format!("queue:{title}"));
        CurrentMedia {
            id: CurrentMediaId {
                run: Some(RunId::new(1)),
                occurrence: occurrence.clone(),
            },
            occurrence: Arc::new(QueueOccurrence {
                occurrence,
                item: QueueItem {
                    media_uri: library::source_entity_uri(
                        &library::SourceId::new("source"),
                        "track",
                        title,
                    ),
                    title: title.to_string(),
                    artist: "Artist".to_string(),
                    album: "Album".to_string(),
                    album_display_artist: None,
                    artwork_binding: None,
                    duration_millis: 180_000,
                    disc_number: None,
                    track_number: None,
                    year: Some(2026),
                    release_date: None,
                    source_format: Some("audio/mpeg".to_string()),
                    musicbrainz_recording_id: None,
                    musicbrainz_release_track_id: None,
                    musicbrainz_album_id: None,
                    musicbrainz_release_group_id: None,
                    primary_artist_musicbrainz_id: None,
                },
                canonical_position: 0,
                source_index: None,
                playlist_entry_id: None,
                provenance: Provenance::Manual,
            }),
        }
    }

    fn playback_view(
        _source: library::SourceKey,
        current: Option<CurrentMedia>,
        position_millis: u64,
    ) -> PlaybackView {
        let current_occurrence = current.as_ref().map(|media| media.id.occurrence.clone());
        PlaybackView {
            queue_loading: false,
            queue_window: Vec::new(),
            queue: QueueSummaryView {
                revision: 1,
                total: usize::from(current.is_some()),
                current_occurrence,
                current_index: current.as_ref().map(|_| 0),
                next_occurrence: None,
                can_next: false,
            },
            transport: TransportView {
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

pub fn fullscreen_equalizer_icon() -> gtk::DrawingArea {
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
