use super::equalizer::EqualizerSurface;
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
pub const FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE: i32 = 96;

#[derive(Clone, Copy, Default)]
enum PaneLayout {
    #[default]
    Both,
    First,
    Second,
}

impl PaneLayout {
    fn next(self, collapsed: bool) -> Self {
        match self {
            Self::Both if collapsed => Self::Second,
            Self::Both => Self::First,
            Self::First => Self::Second,
            Self::Second if collapsed => Self::First,
            Self::Second => Self::Both,
        }
    }

    fn visibility(self, collapsed: bool) -> (bool, bool) {
        match self {
            Self::Both => (true, !collapsed),
            Self::First => (true, false),
            Self::Second => (false, true),
        }
    }

    fn icon_name(self) -> &'static str {
        match self {
            Self::Both => "rufin-view-dual-symbolic",
            Self::First => "rufin-pane-left-focus-symbolic",
            Self::Second => "rufin-pane-right-focus-symbolic",
        }
    }
}

pub struct FullscreenPlayerParts {
    pub root: gtk::Overlay,
    pub visible: Cell<bool>,
    pub animation_tick: RefCell<Option<gtk::TickCallbackId>>,
    pub slide_offset: Rc<Cell<i32>>,
    pub close_button: gtk::Button,
    left_pane: gtk::Box,
    right_pane: gtk::Box,
    hero: gtk::Box,
    hero_content: gtk::Box,
    details: gtk::Box,
    details_scroll: gtk::ScrolledWindow,
    pub lyrics_enabled: Rc<Cell<bool>>,
    pub visualizer_enabled: Rc<Cell<bool>>,
    focus_button: gtk::ToggleButton,
    focus_labels: [gtk::Label; 2],
    background: crate::fullscreen_background::FullscreenBackground,
    bar_tint: gtk::CssProvider,
    bar_color: Cell<Option<[f32; 3]>>,
    experience: gtk::Overlay,
    cover_button: gtk::Button,
    collapsed: Cell<bool>,
    pane_layout: Cell<PaneLayout>,
    pub pane_button: gtk::Button,
    customize_button: gtk::MenuButton,
    visualizer_panel: gtk::Box,
    pub cover: ArtworkTile,
    pub title: gtk::Label,
    pub artist: gtk::Label,
    pub album: gtk::Label,
    pub artist_links: RefCell<Option<ui_shared::detail_links::DetailLinkBinding>>,
    pub album_links: RefCell<Option<ui_shared::detail_links::DetailLinkBinding>>,
    pub meta: gtk::FlowBox,
    pub stack: adw::ViewStack,
    pub lyrics_host: gtk::Box,
    pub queue_panel: gtk::Box,
    pub queue_loading: adw::Spinner,
    pub equalizer: EqualizerSurface,
    related_list: gtk::Box,
    related_status: gtk::Label,
    related_retry: gtk::Button,
    related_empty: gtk::Label,
    related_error: gtk::Label,
    related_loading: gtk::Label,
    related_seed: RefCell<Option<String>>,
    related_task: RefCell<Option<tokio::task::AbortHandle>>,
}

impl Drop for FullscreenPlayerParts {
    fn drop(&mut self) {
        if let Some(task) = self.related_task.get_mut().take() {
            task.abort();
        }
    }
}

impl FullscreenPlayerParts {
    pub fn set_slide_offset(&self, offset: i32) {
        if self.slide_offset.replace(offset) != offset
            && let Some(parent) = self.root.parent()
        {
            parent.queue_allocate();
        }
    }

    pub fn has_experience(&self) -> bool {
        self.lyrics_enabled.get() || self.visualizer_enabled.get()
    }

    pub fn attach_lyrics_pane(&self, pane: &crate::lyrics::LyricsPane) {
        self.lyrics_host.append(pane.widget());
        pane.settings_controls.prepend(&self.focus_button);
    }

    pub fn experience_visible(&self) -> bool {
        self.has_experience()
            && (self.focus_button.is_active()
                || self.pane_layout.get().visibility(self.collapsed.get()).0)
    }

    fn apply_mode(&self) {
        let has_experience = self.has_experience();
        self.experience.set_visible(has_experience);
        self.lyrics_host.set_visible(has_experience);
        self.visualizer_panel
            .set_visible(self.visualizer_enabled.get());
        self.update_hero_layout();
    }

    pub fn update_hero_layout(&self) {
        let has_experience = self.has_experience();
        if has_experience {
            self.hero_content
                .set_orientation(gtk::Orientation::Horizontal);
            self.hero_content.set_halign(gtk::Align::Fill);
            self.hero_content.set_valign(gtk::Align::Fill);
            self.cover_button.set_halign(gtk::Align::Start);
            self.cover_button.set_valign(gtk::Align::Center);
            self.details.set_halign(gtk::Align::Fill);
            self.details.set_valign(gtk::Align::Center);
            self.details_scroll.set_vexpand(true);
            self.details_scroll.set_valign(gtk::Align::Fill);
            for label in [&self.title, &self.artist, &self.album] {
                label.set_halign(gtk::Align::Start);
                label.set_xalign(0.0);
                label.set_justify(gtk::Justification::Left);
            }
            self.meta.set_halign(gtk::Align::Start);
        } else {
            self.hero_content
                .set_orientation(gtk::Orientation::Vertical);
            self.hero_content.set_halign(gtk::Align::Center);
            self.hero_content.set_valign(gtk::Align::Center);
            self.cover_button.set_halign(gtk::Align::Center);
            self.cover_button.set_valign(gtk::Align::Center);
            self.details.set_halign(gtk::Align::Center);
            self.details.set_valign(gtk::Align::Center);
            self.details_scroll.set_vexpand(false);
            self.details_scroll.set_valign(gtk::Align::Center);
            for label in [&self.title, &self.artist, &self.album] {
                label.set_halign(gtk::Align::Center);
                label.set_xalign(0.5);
                label.set_justify(gtk::Justification::Center);
            }
            self.meta.set_halign(gtk::Align::Center);
        }
    }
}

pub fn build_fullscreen_player(visualizer_area: &gtk::DrawingArea) -> FullscreenPlayerParts {
    let resource = crate::ui_resource::FULLSCREEN_PLAYER_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        root: gtk::Overlay,
        close_button: gtk::Button,
        left_pane: gtk::Box,
        right_pane: gtk::Box,
        focus_button: gtk::ToggleButton,
        focus_expand_label: gtk::Label,
        focus_restore_label: gtk::Label,
        body: gtk::Box,
        cover_button: gtk::Button,
        pane_button: gtk::Button,
        customize_button: gtk::MenuButton,
        experience: gtk::Overlay,
        hero: gtk::Box,
        hero_content: gtk::Box,
        details: gtk::Box,
        details_scroll: gtk::ScrolledWindow,
        title: gtk::Label,
        artist: gtk::Label,
        album: gtk::Label,
        meta: gtk::FlowBox,
        stack: adw::ViewStack,
        queue_tab: gtk::ToggleButton,
        related_tab: gtk::ToggleButton,
        equalizer_tab: gtk::ToggleButton,
        queue_panel: gtk::Box,
        queue_loading: adw::Spinner,
        lyrics_host: gtk::Box,
        visualizer_panel: gtk::Box,
        equalizer_panel: gtk::ScrolledWindow,
        related_panel: gtk::Box,
        related_list: gtk::Box,
        related_status: gtk::Label,
        related_retry: gtk::Button,
        related_empty: gtk::Label,
        related_error: gtk::Label,
        related_loading: gtk::Label,
    });
    let background = crate::fullscreen_background::FullscreenBackground::new();
    root.set_child(None::<&gtk::Widget>);
    root.set_child(Some(&background));
    root.add_overlay(&body);
    root.set_measure_overlay(&body, true);
    background.set_visible(false);
    let cover = ArtworkTile::new(FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE);
    cover.area.add_css_class("fullscreen-player-cover");
    cover.area.set_valign(gtk::Align::Start);
    cover_button.set_child(Some(&cover.area));
    let lyrics_enabled = Rc::new(Cell::new(true));
    let visualizer_enabled = Rc::new(Cell::new(false));
    hero.append(&hero_content);
    stack.add_titled(&queue_panel, Some("queue"), &tr("Queue"));
    stack.add_titled(&related_panel, Some("related"), &tr("Related"));
    let equalizer = EqualizerSurface::new(&EqualizerSettings::default());
    equalizer.root.set_valign(gtk::Align::Center);
    equalizer.set_band_height_request(500);
    equalizer_panel.set_child(Some(&equalizer.root));
    stack.add_titled(&equalizer_panel, Some("equalizer"), &tr("Equalizer"));
    stack.set_visible_child_name("queue");
    visualizer_panel.append(visualizer_area);
    let experience_base = gtk::Box::new(gtk::Orientation::Vertical, 0);
    experience_base.set_hexpand(true);
    experience_base.set_vexpand(true);
    experience.set_child(Some(&experience_base));
    experience.add_overlay(&visualizer_panel);
    experience.set_measure_overlay(&visualizer_panel, false);
    experience.add_overlay(&lyrics_host);
    experience.set_measure_overlay(&lyrics_host, false);
    connect_fullscreen_player_switcher(
        &stack,
        [
            (queue_tab, "queue"),
            (related_tab, "related"),
            (equalizer_tab, "equalizer"),
        ],
    );
    let parts = FullscreenPlayerParts {
        root,
        visible: Cell::new(false),
        animation_tick: RefCell::new(None),
        slide_offset: Rc::new(Cell::new(0)),
        close_button,
        left_pane,
        right_pane,
        hero,
        hero_content,
        details,
        details_scroll,
        focus_button,
        background,
        bar_tint: gtk::CssProvider::new(),
        bar_color: Cell::new(None),
        focus_labels: [focus_expand_label, focus_restore_label],
        lyrics_enabled,
        visualizer_enabled,
        experience,
        cover_button,
        collapsed: Cell::new(false),
        pane_layout: Cell::new(PaneLayout::default()),
        pane_button,
        customize_button,
        visualizer_panel,
        cover,
        title,
        artist,
        album,
        artist_links: RefCell::new(None),
        album_links: RefCell::new(None),
        meta,
        stack,
        lyrics_host,
        queue_panel,
        queue_loading,
        equalizer,
        related_list,
        related_status,
        related_retry,
        related_empty,
        related_error,
        related_loading,
        related_seed: RefCell::new(None),
        related_task: RefCell::new(None),
    };
    parts.apply_mode();
    parts
}

fn connect_fullscreen_player_switcher(
    stack: &adw::ViewStack,
    tabs: [(gtk::ToggleButton, &'static str); 3],
) {
    let mut selection = Vec::new();
    for (button, page) in tabs {
        let page_stack = stack.downgrade();
        button.connect_clicked(move |_| {
            if let Some(stack) = page_stack.upgrade() {
                stack.set_visible_child_name(page);
            }
        });
        selection.push((button.downgrade(), page));
    }
    stack.connect_visible_child_name_notify(move |stack| {
        for (button, name) in &selection {
            if let Some(button) = button.upgrade() {
                button.set_active(stack.visible_child_name().as_deref() == Some(*name));
            }
        }
    });
}

use crate::{bottom::BOTTOM_PLAYER_HEIGHT, state::NowPlayingPresentation};
use gtk::glib;
use ui_shared::artwork::cover_fetch_size_for_display;

fn fullscreen_settings_popover(shell: &Rc<crate::PlayerUi>) -> gtk::Popover {
    let resource = crate::ui_resource::FULLSCREEN_SETTINGS_RESOURCE;
    let builder = ui_shared::ui_resource::builder(resource);
    ui_shared::objects!(builder, resource, {
        popover: gtk::Popover,
        lyrics: adw::SwitchRow,
        visualizer: adw::SwitchRow,
        dynamic_background: adw::SwitchRow,
        background_image: adw::SwitchRow,
    });
    let settings = shell.settings.current.borrow();
    let active = [
        settings.fullscreen_lyrics_visible,
        settings.fullscreen_visualizer_visible,
        settings.fullscreen_dynamic_background,
        settings.fullscreen_background_image,
    ];
    drop(settings);
    background_image.set_sensitive(active[2]);
    let image_row = background_image.downgrade();
    dynamic_background.connect_active_notify(move |row| {
        if let Some(image_row) = image_row.upgrade() {
            image_row.set_sensitive(row.is_active());
        }
    });
    for (index, row) in [lyrics, visualizer, dynamic_background, background_image]
        .into_iter()
        .enumerate()
    {
        row.set_active(active[index]);
        let weak = Rc::downgrade(shell);
        row.connect_active_notify(move |row| {
            let Some(shell) = weak.upgrade() else {
                return;
            };
            shell
                .settings
                .update_app_settings("fullscreen display", |settings| {
                    let value = match index {
                        0 => &mut settings.fullscreen_lyrics_visible,
                        1 => &mut settings.fullscreen_visualizer_visible,
                        2 => &mut settings.fullscreen_dynamic_background,
                        _ => &mut settings.fullscreen_background_image,
                    };
                    if *value == row.is_active() {
                        return false;
                    }
                    *value = row.is_active();
                    true
                });
            shell.apply_fullscreen_display_settings();
        });
    }
    popover
}
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
    if let Some(window) = shell.window.upgrade() {
        gtk::style_context_add_provider_for_display(
            &gtk::prelude::WidgetExt::display(&window),
            &shell.views.fullscreen_player.bar_tint,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let weak = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .related_retry
        .connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                shell
                    .views
                    .fullscreen_player
                    .related_seed
                    .borrow_mut()
                    .take();
                shell.refresh_related_tracks();
            }
        });
    let close_shell = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .close_button
        .connect_clicked(move |_| {
            if let Some(shell) = close_shell.upgrade() {
                shell.close_fullscreen_player();
            }
        });

    let weak = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .customize_button
        .set_create_popup_func(move |button| {
            if let Some(shell) = weak.upgrade() {
                button.set_popover(Some(&fullscreen_settings_popover(&shell)));
            }
        });
    let weak = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .focus_button
        .connect_toggled(move |button| {
            button.set_icon_name(if button.is_active() {
                "rufin-view-restore-corners-symbolic"
            } else {
                "rufin-view-fullscreen-corners-symbolic"
            });
            let Some(shell) = weak.upgrade() else {
                return;
            };
            let label =
                shell.views.fullscreen_player.focus_labels[usize::from(button.is_active())].text();
            button.set_tooltip_text(Some(&label));
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            shell.apply_fullscreen_responsive_layout();
            shell.sync_fullscreen_surfaces();
        });
    let weak = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .cover
        .drag_paintable_source()
        .connect_paintable_notify(move |_| {
            if let Some(shell) = weak.upgrade() {
                shell.refresh_fullscreen_background();
            }
        });
    shell.apply_fullscreen_display_settings();
    let weak = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .pane_button
        .connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                let parts = &shell.views.fullscreen_player;
                parts
                    .pane_layout
                    .set(parts.pane_layout.get().next(parts.collapsed.get()));
                shell.apply_fullscreen_responsive_layout();
                shell.sync_fullscreen_surfaces();
            }
        });
    let weak = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .cover_button
        .connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                let presentation =
                    NowPlayingPresentation::new(shell.selected_playback().as_deref());
                (shell.present_full_artwork)(presentation.artwork);
            }
        });

    let key_shell = Rc::downgrade(shell);
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, key, _, _| {
        let Some(key_shell) = key_shell.upgrade() else {
            return glib::Propagation::Proceed;
        };
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

    let queue_tab_shell = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .stack
        .connect_visible_child_name_notify(move |stack| {
            let Some(queue_tab_shell) = queue_tab_shell.upgrade() else {
                return;
            };
            if !queue_tab_shell.fullscreen_player_visible() {
                return;
            }
            if stack.visible_child_name().as_deref() == Some("queue") {
                queue_tab_shell.schedule_queue_panel_render();
            }
            queue_tab_shell.refresh_related_tracks();
        });

    let equalizer_shell = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .equalizer
        .connect_changed(move |equalizer| {
            let Some(equalizer_shell) = equalizer_shell.upgrade() else {
                return;
            };
            equalizer_shell.update_playback_settings(|settings| {
                settings.equalizer = equalizer.clone();
            });
        });
}

impl crate::PlayerUi {
    fn refresh_related_tracks(self: &Rc<Self>) {
        let parts = &self.views.fullscreen_player;
        if !self.fullscreen_player_visible()
            || parts.stack.visible_child_name().as_deref() != Some("related")
            || !parts.right_pane.is_visible()
        {
            return;
        }
        let seed = self.selected_playback().and_then(|player| {
            player
                .transport
                .current
                .as_ref()
                .map(|track| track.media_uri.clone())
        });
        if *parts.related_seed.borrow() == seed {
            return;
        }
        if let Some(task) = parts.related_task.borrow_mut().take() {
            task.abort();
        }
        *parts.related_seed.borrow_mut() = seed.clone();
        while let Some(child) = parts.related_list.first_child() {
            parts.related_list.remove(&child);
        }
        parts.related_status.set_visible(true);
        parts.related_retry.set_visible(false);
        let Some(seed) = seed else {
            parts.related_status.set_text(&parts.related_empty.text());
            return;
        };
        parts.related_status.set_text(&parts.related_loading.text());
        let candidates = self
            .playback_handles
            .radio
            .candidates(library::RadioSeed::Track(seed.clone()), 20);
        let database = self.database.clone();
        let task = self.runtime.spawn(async move {
            let uris = candidates.await?;
            database
                .track_rows_by_uri(&uris, &library::ReadCancellation::new())
                .await
                .map_err(|error| error.to_string())
        });
        *parts.related_task.borrow_mut() = Some(task.abort_handle());
        let task_id = task.id();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let result = task.await;
            let Some(shell) = weak.upgrade() else {
                return;
            };
            let parts = &shell.views.fullscreen_player;
            if parts
                .related_task
                .borrow()
                .as_ref()
                .map(tokio::task::AbortHandle::id)
                != Some(task_id)
            {
                return;
            }
            parts.related_task.borrow_mut().take();
            match result {
                Ok(Ok(items)) => {
                    parts.related_status.set_text(&parts.related_empty.text());
                    parts.related_status.set_visible(items.is_empty());
                    if !items.is_empty() {
                        let view = (shell.related_tracks_view)(items);
                        parts.related_list.append(&view);
                    }
                }
                _ => {
                    parts.related_status.set_text(&parts.related_error.text());
                    parts.related_retry.set_visible(true);
                }
            }
        });
    }

    fn apply_fullscreen_display_settings(self: &Rc<Self>) {
        let settings = self.settings.current.borrow();
        let parts = &self.views.fullscreen_player;
        parts.lyrics_enabled.set(settings.fullscreen_lyrics_visible);
        parts
            .visualizer_enabled
            .set(settings.fullscreen_visualizer_visible);
        drop(settings);
        parts.apply_mode();
        self.apply_fullscreen_responsive_layout();
        self.refresh_fullscreen_background();
        self.sync_fullscreen_surfaces();
    }

    fn refresh_fullscreen_background(&self) {
        let parts = &self.views.fullscreen_player;
        if !self.fullscreen_player_visible() || parts.animation_tick.borrow().is_some() {
            return;
        }
        let settings = self.settings.current.borrow();
        let dynamic = settings.fullscreen_dynamic_background;
        parts.background.set_visible(dynamic);
        if dynamic {
            parts.root.add_css_class("artwork-background");
            parts.background.update(
                parts.cover.drag_paintable_source().paintable(),
                settings.fullscreen_background_image,
            );
        } else {
            parts.root.remove_css_class("artwork-background");
        }
        if let Some(window) = self.window.upgrade() {
            if dynamic && self.fullscreen_player_visible() {
                let color = parts.background.color();
                if parts.bar_color.replace(Some(color)) != Some(color) {
                    let [r, g, b] = color;
                    parts.bar_tint.load_from_string(&format!(
                        "window {{ --fullscreen-artwork-color: {}; }}",
                        gtk::gdk::RGBA::new(r, g, b, 1.0)
                    ));
                }
                window.add_css_class("fullscreen-artwork-bars");
            } else if !self.fullscreen_player_visible() && parts.animation_tick.borrow().is_none() {
                window.remove_css_class("fullscreen-artwork-bars");
            }
        }
    }

    pub fn open_fullscreen_player(self: &Rc<Self>) {
        let Some(player) = self.selected_playback().as_deref().cloned() else {
            return;
        };
        if player.transport.current.is_none() {
            return;
        }
        self.views.fullscreen_player.visible.set(true);
        self.animate_fullscreen_player(true);
        self.apply_fullscreen_display_settings();
        let presentation = NowPlayingPresentation::new(Some(&player));
        self.apply_fullscreen_now_playing_text(&presentation);
        self.apply_fullscreen_responsive_layout();
        self.apply_fullscreen_now_playing_cover(&presentation);
        self.schedule_queue_panel_render();
        self.sync_fullscreen_surfaces();
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
        self.refresh_related_tracks();
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

    fn apply_fullscreen_responsive_layout(self: &Rc<Self>) {
        let (width, height) = {
            let root = &self.views.fullscreen_player.root;
            if root.width() > 0 && root.height() > 0 {
                (root.width(), root.height())
            } else {
                (
                    self.window.upgrade().map_or(0, |window| window.width()),
                    self.window.upgrade().map_or(0, |window| window.height()),
                )
            }
        };
        self.apply_fullscreen_responsive_layout_for_size(width, height);
    }

    pub fn apply_fullscreen_responsive_layout_for_size(self: &Rc<Self>, width: i32, height: i32) {
        let parts = &self.views.fullscreen_player;
        let collapsed = width < 800;
        let changed = parts.collapsed.replace(collapsed) != collapsed;
        let (first, second) = parts.pane_layout.get().visibility(collapsed);
        let has_experience = parts.has_experience();
        let focused = parts.focus_button.is_active() && has_experience;
        parts
            .pane_button
            .set_icon_name(parts.pane_layout.get().icon_name());
        parts.left_pane.set_visible(focused || first);
        parts.right_pane.set_visible(!focused && second);
        parts.hero.set_visible(!focused);
        parts.experience.set_visible(has_experience);
        parts.update_hero_layout();
        let pane_width = if first && second && !focused {
            width.saturating_sub(24) / 2
        } else {
            width.saturating_sub(16)
        };
        if has_experience {
            parts.hero.set_vexpand(false);
            let hero_height = ((height - BOTTOM_PLAYER_HEIGHT).max(1) * 42 / 100).clamp(96, 544);
            if parts.hero.height_request() != hero_height {
                parts.hero.set_height_request(hero_height);
            }
            let max_w = (pane_width.saturating_sub(48) * 40) / 100;
            let max_h = hero_height.saturating_sub(32);
            let cover_size = max_w.min(max_h).clamp(48, 340);
            parts.cover.set_square_size(cover_size);
        } else {
            parts.hero.set_vexpand(true);
            parts.hero.set_height_request(-1);
            let max_cover_from_height = height.saturating_sub(BOTTOM_PLAYER_HEIGHT + 228);
            let max_cover_from_width = pane_width.saturating_sub(48);
            let cover_size = max_cover_from_height
                .min(max_cover_from_width)
                .min((height * 50) / 100)
                .clamp(96, 480);
            parts.cover.set_square_size(cover_size);
        }
        if changed {
            self.sync_fullscreen_surfaces();
        }
    }

    fn sync_fullscreen_surfaces(self: &Rc<Self>) {
        self.sync_visible_lyrics_surfaces();
        self.refocus_fullscreen_lyrics_position();
        self.sync_visualizer_state();
        self.refresh_related_tracks();
    }

    fn apply_fullscreen_now_playing_cover(self: &Rc<Self>, presentation: &NowPlayingPresentation) {
        let cover_size = self.fullscreen_player_cover_size();
        self.views
            .fullscreen_player
            .cover
            .set_square_size(cover_size);
        if presentation.current {
            let fetch_size = cover_fetch_size_for_display(512);
            self.artwork.bind_playback_artwork_tile(
                &self.views.fullscreen_player.cover,
                presentation.artwork.clone(),
                512,
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
        let controls = &self.views.player_controls;
        let fullscreen = &self.views.fullscreen_player;
        for (source, target) in [
            (&controls.artist_links, &fullscreen.artist_links),
            (&controls.album_links, &fullscreen.album_links),
        ] {
            if let (Some(source), Some(target)) =
                (source.borrow().as_ref(), target.borrow().as_ref())
            {
                target.bind(source.links());
            }
        }
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
        self.views
            .fullscreen_player
            .cover
            .area
            .width_request()
            .max(1)
    }

    pub fn clear_fullscreen_player_cover(self: &Rc<Self>) {
        self.artwork
            .clear_artwork_tile(&self.views.fullscreen_player.cover);
    }

    fn refocus_fullscreen_lyrics_position(&self) {
        if !self.fullscreen_lyrics_surface_visible() {
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
        if opening {
            self.views.fullscreen_player.background.set_visible(false);
            root.remove_css_class("artwork-background");
        }
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
                    if let Some(window) = tick_shell.window.upgrade() {
                        window.remove_css_class("fullscreen-artwork-bars");
                    }
                }
                root.set_visible(opening);
                tick_shell
                    .views
                    .fullscreen_player
                    .animation_tick
                    .borrow_mut()
                    .take();
                tick_shell.refresh_fullscreen_background();
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
