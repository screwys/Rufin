use super::equalizer::EqualizerSurface;
use crate::widgets::artwork::ArtworkTile;
use crate::widgets::swipe::{SWIPE_ACTION_DISTANCE, SwipeAxis, install_touch_swipe};
use adw::prelude::*;
use localization::tr;
use playback::PlaybackView;
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};
pub const FULLSCREEN_PLAYER_OPEN_TRANSITION_MS: u32 = 420;
pub const FULLSCREEN_PLAYER_CLOSE_TRANSITION_MS: u32 = 320;
pub const FULLSCREEN_PLAYER_DEFAULT_COVER_SIZE: i32 = 96;
/// How far the fullscreen player is dragged down before letting go closes it.
const FULLSCREEN_PLAYER_DRAG_CLOSE_DISTANCE: f64 = 120.0;
/// Below this height the lyrics and visualizer give way, and the cover sits beside the details.
const FULLSCREEN_PLAYER_EXPERIENCE_MIN_HEIGHT: i32 = 320;
/// Beside the cover, the album and then the facts row show once the pane has room for them.
const FULLSCREEN_PLAYER_ALBUM_MIN_HEIGHT: i32 = 112;
const FULLSCREEN_PLAYER_META_MIN_HEIGHT: i32 = 140;

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

    fn icon_name(self, collapsed: bool) -> &'static str {
        match self {
            Self::Both if collapsed => "rufin-pane-left-focus-symbolic",
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
    topbar: gtk::Box,
    /// Holds the controller and output buttons while the window is too narrow for them.
    pub playback_targets_host: gtk::Box,
    /// The tall mini player shows only the cover, with what is playing in the window's title; the
    /// short one shows the cover beside the details.
    pub mini: Cell<super::MiniPlayer>,
    /// Too short for the lyrics and visualizer under the cover: the cover sits beside the details.
    single: Cell<bool>,
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
    background: crate::player::fullscreen_background::FullscreenBackground,
    bar_tint: gtk::CssProvider,
    bar_color: Cell<Option<[f32; 3]>>,
    experience: gtk::Overlay,
    cover_button: gtk::Button,
    collapsed: Cell<bool>,
    pane_layout: Cell<PaneLayout>,
    pub pane_button: gtk::Button,
    customize_button: gtk::Button,
    visualizer_panel: gtk::Box,
    pub cover: ArtworkTile,
    pub title: gtk::Label,
    pub artist: gtk::Label,
    pub album: gtk::Label,
    album_row: gtk::Widget,
    /// Whether the pane has room for the facts row.
    meta_fits: Cell<bool>,
    pub artist_links: RefCell<Option<crate::widgets::detail_links::DetailLinkBinding>>,
    pub album_links: RefCell<Option<crate::widgets::detail_links::DetailLinkBinding>>,
    pub meta: gtk::FlowBox,
    pub stack: adw::ViewStack,
    pub lyrics_host: gtk::Box,
    pub queue_panel: gtk::Box,
    pub(crate) queue_header: crate::widgets::table_header::ColumnViewHeader,
    pub queue_loading: adw::Spinner,
    pub equalizer: OnceCell<EqualizerSurface>,
    equalizer_panel: gtk::ScrolledWindow,
    related_list: gtk::Box,
    related_status: gtk::Label,
    related_retry: gtk::Button,
    related_empty: gtk::Label,
    related_error: gtk::Label,
    related_loading: adw::Spinner,
    related_seed: RefCell<Option<String>>,
    related_display_changed: RefCell<Option<Rc<dyn Fn()>>>,
    pub(super) queue_width_fit: RefCell<Option<crate::widgets::table_sizing::ColumnViewWidthFit>>,
    related_task: RefCell<Option<(library::ReadCancellation, tokio::task::AbortHandle)>>,
}

impl Drop for FullscreenPlayerParts {
    fn drop(&mut self) {
        if let Some((cancellation, task)) = self.related_task.get_mut().take() {
            cancellation.cancel();
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
        self.mini.get() == super::MiniPlayer::Off
            && !self.single.get()
            && (self.lyrics_enabled.get() || self.visualizer_enabled.get())
    }

    /// The facts row shows when there are facts and the pane has room for them.
    fn sync_meta_visible(&self) {
        self.meta
            .set_visible(self.meta_fits.get() && self.meta.first_child().is_some());
    }

    pub fn attach_lyrics_pane(&self, pane: &crate::player::lyrics::LyricsPane) {
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
        if has_experience || self.single.get() {
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
            self.hero_content.set_halign(gtk::Align::Fill);
            self.hero_content.set_valign(gtk::Align::Center);
            self.cover_button.set_halign(gtk::Align::Center);
            self.cover_button.set_valign(gtk::Align::Center);
            self.details.set_halign(gtk::Align::Fill);
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

pub fn build_fullscreen_player(
    visualizer_area: &crate::player::visualizer::Visualizer,
) -> FullscreenPlayerParts {
    let resource = "player/fullscreen.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        root: gtk::Overlay,
        close_button: gtk::Button,
        topbar: gtk::Box,
        playback_targets_host: gtk::Box,
        left_pane: gtk::Box,
        right_pane: gtk::Box,
        focus_button: gtk::ToggleButton,
        focus_expand_label: gtk::Label,
        focus_restore_label: gtk::Label,
        body: gtk::Box,
        cover_button: gtk::Button,
        pane_button: gtk::Button,
        customize_button: gtk::Button,
        experience: gtk::Overlay,
        hero: gtk::Box,
        hero_content: gtk::Box,
        details: gtk::Box,
        details_scroll: gtk::ScrolledWindow,
        title_slide: gtk::ScrolledWindow, title_viewport: gtk::Viewport,
        artist_slide: gtk::ScrolledWindow, artist_viewport: gtk::Viewport,
        album_slide: gtk::ScrolledWindow, album_viewport: gtk::Viewport,
        title: gtk::Label,
        artist: gtk::Label,
        album: gtk::Label,
        album_fade: gtk::Widget,
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
        related_panel: gtk::Overlay,
        related_list: gtk::Box,
        related_status: gtk::Label,
        related_retry: gtk::Button,
        related_empty: gtk::Label,
        related_error: gtk::Label,
        related_loading: adw::Spinner,
    });
    let background = crate::player::fullscreen_background::FullscreenBackground::new();
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
    crate::widgets::text_slide::install(&title_slide, &title_viewport, &title);
    crate::widgets::text_slide::install(&artist_slide, &artist_viewport, &artist);
    crate::widgets::text_slide::install(&album_slide, &album_viewport, &album);
    let parts = FullscreenPlayerParts {
        root,
        visible: Cell::new(false),
        animation_tick: RefCell::new(None),
        slide_offset: Rc::new(Cell::new(0)),
        close_button,
        topbar,
        playback_targets_host,
        mini: Cell::new(super::MiniPlayer::Off),
        single: Cell::new(false),
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
        album_row: album_fade,
        meta_fits: Cell::new(true),
        artist_links: RefCell::new(None),
        album_links: RefCell::new(None),
        meta,
        stack,
        lyrics_host,
        queue_panel,
        queue_header: crate::widgets::table_header::ColumnViewHeader::new(),
        queue_loading,
        equalizer: OnceCell::new(),
        equalizer_panel,
        related_list,
        related_status,
        related_retry,
        related_empty,
        related_error,
        related_loading,
        related_seed: RefCell::new(None),
        related_display_changed: RefCell::new(None),
        queue_width_fit: RefCell::new(None),
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

use crate::player::{
    bottom::{BOTTOM_PLAYER_HEIGHT, BOTTOM_PLAYER_STRIP_HEIGHT},
    state::NowPlayingPresentation,
};
use gtk::glib;

fn present_fullscreen_settings(shell: &Rc<crate::player::PlayerUi>) {
    let Some(app) = shell.shell() else {
        return;
    };
    let window = app.chrome.window.clone();
    let key = rufin_core::settings::LibraryListKey::FullscreenTracks;
    let resource = "player/fullscreen_settings.ui";
    let builder = crate::ui_resource::builder(resource);
    crate::objects!(builder, resource, {
        dialog: adw::PreferencesDialog,
        page: adw::PreferencesPage,
        fields_group: adw::PreferencesGroup,
    });
    dialog.set_content_width(crate::widgets::layout::large_popup_content_width(560));
    dialog.set_content_height(crate::widgets::layout::large_popup_content_height(
        window.height(),
        640,
    ));
    let rows = Rc::new(RefCell::new(Vec::new()));
    let weak = Rc::downgrade(shell);
    let changed: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(shell) = weak.upgrade() {
            shell.refresh_fullscreen_columns();
        }
    });
    crate::widgets::library_field_editor::populate_library_field_rows(
        &app.settings,
        changed,
        key,
        &fields_group,
        &rows,
    );
    let weak = Rc::downgrade(shell);
    let display = crate::widgets::display::DisplayEditor::new(
        &app.settings.current.borrow().library_list(key).display,
        move |display| {
            if let Some(shell) = weak.upgrade()
                && let Some(app) = shell.shell()
            {
                app.settings.update_library_list_settings(key, |settings| {
                    settings.display = display.clone()
                });
                shell.apply_fullscreen_display_settings();
                shell.refresh_fullscreen_columns();
            }
        },
    );
    display.set_layout(Some(rufin_core::settings::LibraryLayout::Row));
    page.remove(&fields_group);
    page.add(&display.group);
    page.add(&fields_group);
    crate::widgets::popup::present_light_dismiss_dialog(&dialog, &window);
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

pub fn connect_fullscreen_player_controls(shell: &Rc<crate::player::PlayerUi>) {
    let Some(app) = shell.shell() else {
        return;
    };
    {
        let window = app.chrome.window.clone();
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

    // Dragging the top bar or the cover down follows the finger and closes the fullscreen player;
    // swiping the cover sideways skips like the bar does.
    for (target, skips) in [
        (
            shell
                .views
                .fullscreen_player
                .topbar
                .clone()
                .upcast::<gtk::Widget>(),
            false,
        ),
        (shell.views.fullscreen_player.hero.clone().upcast(), true),
    ] {
        let claims_shell = Rc::downgrade(shell);
        let moved_shell = Rc::downgrade(shell);
        let released_shell = Rc::downgrade(shell);
        install_touch_swipe(
            &target,
            move |axis, _, dy| {
                let Some(shell) = claims_shell.upgrade() else {
                    return false;
                };
                match axis {
                    SwipeAxis::Horizontal => skips,
                    SwipeAxis::Vertical => {
                        dy > 0.0
                            && shell.mini.get() == super::MiniPlayer::Off
                            && shell
                                .views
                                .fullscreen_player
                                .animation_tick
                                .borrow()
                                .is_none()
                    }
                }
            },
            move |axis, _, dy| {
                if axis == SwipeAxis::Vertical
                    && let Some(shell) = moved_shell.upgrade()
                {
                    shell
                        .views
                        .fullscreen_player
                        .set_slide_offset(dy.max(0.0).round() as i32);
                }
            },
            move |axis, dx, dy| {
                let Some(shell) = released_shell.upgrade() else {
                    return;
                };
                match axis {
                    SwipeAxis::Horizontal if dx.abs() >= SWIPE_ACTION_DISTANCE => {
                        shell.swipe_track(dx);
                    }
                    SwipeAxis::Horizontal => {}
                    SwipeAxis::Vertical => {
                        let offset = Some(shell.views.fullscreen_player.slide_offset.get());
                        if dy >= FULLSCREEN_PLAYER_DRAG_CLOSE_DISTANCE {
                            shell.hide_fullscreen_player(offset, true);
                        } else {
                            shell.animate_fullscreen_player(true, offset, true);
                        }
                    }
                }
            },
        );
    }

    let weak = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .customize_button
        .connect_clicked(move |_| {
            if let Some(shell) = weak.upgrade() {
                present_fullscreen_settings(&shell);
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
                shell.present_full_artwork(presentation.artwork);
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
    app.chrome.window.add_controller(key);

    let queue_tab_shell = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .stack
        .connect_visible_child_name_notify(move |_| {
            let Some(queue_tab_shell) = queue_tab_shell.upgrade() else {
                return;
            };
            if !queue_tab_shell.fullscreen_player_visible() {
                return;
            }
            queue_tab_shell.refresh_related_tracks();
        });

    let equalizer_shell = Rc::downgrade(shell);
    shell
        .views
        .fullscreen_player
        .equalizer_panel
        .connect_map(move |panel| {
            let Some(shell) = equalizer_shell.upgrade() else {
                return;
            };
            shell.views.fullscreen_player.equalizer.get_or_init(|| {
                let equalizer = EqualizerSurface::new(&app);
                panel.set_child(Some(&equalizer.root));
                equalizer
            });
        });
}

impl crate::player::PlayerUi {
    fn refresh_related_tracks(self: &Rc<Self>) {
        let Some(shell) = self.shell() else {
            return;
        };
        let parts = &self.views.fullscreen_player;
        if !self.fullscreen_player_visible() {
            return;
        }
        let seed = self.selected_playback().and_then(|player| {
            player
                .transport
                .current
                .as_ref()
                .map(|track| track.row.track.media_uri.clone())
        });
        if *parts.related_seed.borrow() == seed {
            return;
        }
        if let Some((cancellation, task)) = parts.related_task.borrow_mut().take() {
            cancellation.cancel();
            task.abort();
        }
        *parts.related_seed.borrow_mut() = seed.clone();
        parts.related_display_changed.borrow_mut().take();
        while let Some(child) = parts.related_list.first_child() {
            parts.related_list.remove(&child);
        }
        parts.related_status.set_visible(seed.is_none());
        parts.related_loading.set_visible(seed.is_some());
        parts.related_retry.set_visible(false);
        let Some(seed) = seed else {
            parts.related_status.set_text(&parts.related_empty.text());
            return;
        };
        let cancellation = library::ReadCancellation::new();
        let candidates = shell.products.playback.candidates(
            library::RadioSeed::Track(seed.clone()),
            20,
            cancellation.clone(),
        );
        let catalog = shell.products.library.clone();
        let read_cancellation = cancellation.clone();
        let task = shell.products.runtime.spawn(async move {
            let items = candidates
                .await?
                .into_iter()
                .map(|key| library::Item::Track { key, entry: None })
                .collect::<Vec<_>>();
            catalog
                .rows_of(&items, true, &read_cancellation)
                .await
                .map(|rows| {
                    rows.into_iter()
                        .filter_map(|row| match row {
                            library::Row::Track(track) => Some(track),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                })
                .map_err(|error| error.to_string())
        });
        *parts.related_task.borrow_mut() = Some((cancellation, task.abort_handle()));
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
                .map(|(_, task)| task.id())
                != Some(task_id)
            {
                return;
            }
            parts.related_task.borrow_mut().take();
            parts.related_loading.set_visible(false);
            match result {
                Ok(Ok(items)) => {
                    parts.related_status.set_text(&parts.related_empty.text());
                    parts.related_status.set_visible(items.is_empty());
                    if !items.is_empty() {
                        let (view, changed) = shell.related_tracks_view(items);
                        parts.related_display_changed.replace(Some(changed));
                        parts.related_list.append(&view);
                    }
                }
                _ => {
                    parts.related_status.set_text(&parts.related_error.text());
                    parts.related_status.set_visible(true);
                    parts.related_retry.set_visible(true);
                }
            }
        });
    }

    fn refresh_fullscreen_columns(self: &Rc<Self>) {
        self.apply_fullscreen_queue_display();
        if let Some(changed) = self
            .views
            .fullscreen_player
            .related_display_changed
            .borrow()
            .as_ref()
        {
            changed();
        }
    }

    pub(crate) fn apply_fullscreen_display_settings(self: &Rc<Self>) {
        let Some(shell) = self.shell() else {
            return;
        };
        let settings = shell.settings.current.borrow();
        let parts = &self.views.fullscreen_player;
        crate::widgets::display::apply(
            &parts.root,
            &settings
                .library_list(rufin_core::settings::LibraryListKey::FullscreenTracks)
                .display,
        );
        parts
            .lyrics_enabled
            .set(settings.player_display.fullscreen_lyrics_visible);
        parts
            .visualizer_enabled
            .set(settings.player_display.fullscreen_visualizer_visible);
        drop(settings);
        parts.apply_mode();
        self.apply_fullscreen_responsive_layout();
        self.refresh_fullscreen_background();
        self.sync_fullscreen_surfaces();
    }

    fn refresh_fullscreen_background(&self) {
        let Some(shell) = self.shell() else {
            return;
        };
        let parts = &self.views.fullscreen_player;
        if !self.fullscreen_player_visible() || parts.animation_tick.borrow().is_some() {
            return;
        }
        let settings = shell.settings.current.borrow();
        let dynamic = settings.player_display.fullscreen_dynamic_background;
        parts.background.set_visible(dynamic);
        if dynamic {
            parts.root.add_css_class("artwork-background");
            parts.background.update(
                parts.cover.drag_paintable_source().paintable(),
                settings.player_display.fullscreen_background_image,
            );
        } else {
            parts.root.remove_css_class("artwork-background");
        }
        {
            let window = shell.chrome.window.clone();
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
        if self
            .selected_playback()
            .is_some_and(|player| player.transport.current.is_some())
        {
            self.show_fullscreen_player(true);
        }
    }

    pub(super) fn show_fullscreen_player(self: &Rc<Self>, animated: bool) {
        self.views.fullscreen_player.visible.set(true);
        self.animate_fullscreen_player(true, None, animated);
        self.apply_fullscreen_display_settings();
        let player = self.selected_playback().as_deref().cloned();
        let presentation = NowPlayingPresentation::new(player.as_ref());
        self.apply_fullscreen_now_playing_text(&presentation);
        self.apply_fullscreen_mode();
        self.apply_fullscreen_now_playing_cover(&presentation);
        self.apply_bottom_player_layout();
        if animated {
            let _focused = self.views.fullscreen_player.close_button.grab_focus();
        }
    }

    pub fn close_fullscreen_player(self: &Rc<Self>) {
        // In the mini players the fullscreen player is the window's content.
        if self.mini.get() == super::MiniPlayer::Off {
            self.hide_fullscreen_player(None, true);
        }
    }

    /// Closes the fullscreen player, sliding it down from `from` when a drag already moved it.
    pub(super) fn hide_fullscreen_player(self: &Rc<Self>, from: Option<i32>, animated: bool) {
        if !self.views.fullscreen_player.visible.replace(false) {
            return;
        }
        let parts = &self.views.fullscreen_player;
        if let Some((cancellation, task)) = parts.related_task.borrow_mut().take() {
            cancellation.cancel();
            task.abort();
            parts.related_seed.borrow_mut().take();
        }
        self.animate_fullscreen_player(false, from, animated);
        self.sync_visualizer_state();
        self.sync_visible_lyrics_surfaces();
        self.apply_bottom_player_layout();
    }

    /// Applies what the fullscreen player shows after the mini player starts or ends.
    pub(super) fn refresh_fullscreen_mode(self: &Rc<Self>) {
        if self.fullscreen_player_visible() {
            self.apply_fullscreen_mode();
        }
    }

    fn apply_fullscreen_mode(self: &Rc<Self>) {
        self.views.fullscreen_player.apply_mode();
        self.apply_fullscreen_responsive_layout();
        self.sync_fullscreen_surfaces();
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
    }

    fn apply_fullscreen_responsive_layout(self: &Rc<Self>) {
        let Some(shell) = self.shell() else {
            return;
        };
        let (width, height) = {
            let root = &self.views.fullscreen_player.root;
            if root.width() > 0 && root.height() > 0 {
                (root.width(), root.height())
            } else {
                (shell.chrome.window.width(), shell.chrome.window.height())
            }
        };
        self.apply_fullscreen_responsive_layout_for_size(width, height);
    }

    pub fn apply_fullscreen_responsive_layout_for_size(self: &Rc<Self>, width: i32, height: i32) {
        let parts = &self.views.fullscreen_player;
        let mini = parts.mini.get() != super::MiniPlayer::Off;
        let tall = parts.mini.get() == super::MiniPlayer::Tall;
        let collapsed = width < 800;
        let single = parts.mini.get() == super::MiniPlayer::Short
            || height - BOTTOM_PLAYER_HEIGHT < FULLSCREEN_PLAYER_EXPERIENCE_MIN_HEIGHT;
        let changed = (parts.collapsed.replace(collapsed) != collapsed)
            | (parts.single.replace(single) != single);
        let (first, second) = if mini {
            (true, false)
        } else {
            parts.pane_layout.get().visibility(collapsed)
        };
        parts.topbar.set_visible(!mini);
        parts.details_scroll.set_visible(!tall);
        parts.cover_button.set_can_target(!mini);
        let has_experience = parts.has_experience();
        let focused = parts.focus_button.is_active() && has_experience;
        parts
            .pane_button
            .set_icon_name(parts.pane_layout.get().icon_name(collapsed));
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
        // The height the pane has above the bar.
        let pane_height = height.saturating_sub(if mini {
            BOTTOM_PLAYER_STRIP_HEIGHT
        } else {
            BOTTOM_PLAYER_HEIGHT + 48
        });
        parts
            .album_row
            .set_visible(!single || pane_height >= FULLSCREEN_PLAYER_ALBUM_MIN_HEIGHT);
        parts
            .meta_fits
            .set(!single || pane_height >= FULLSCREEN_PLAYER_META_MIN_HEIGHT);
        parts.sync_meta_visible();
        if single {
            // The cover takes the pane's height, beside the details.
            parts.hero.set_vexpand(true);
            parts.hero.set_height_request(-1);
            let max_w = (pane_width.saturating_sub(48) * 40) / 100;
            let max_h = pane_height.saturating_sub(32);
            parts.cover.set_square_size(max_w.min(max_h).clamp(48, 340));
        } else if has_experience {
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
            // Leave room for the details under the cover, which the mini player shows in the title.
            let (details_height, height_share, smallest) =
                if tall { (32, 100, 48) } else { (228, 50, 96) };
            let max_cover_from_height =
                height.saturating_sub(BOTTOM_PLAYER_HEIGHT + details_height);
            let max_cover_from_width = pane_width.saturating_sub(48);
            let cover_size = max_cover_from_height
                .min(max_cover_from_width)
                .min((height * height_share) / 100)
                .clamp(smallest, 480);
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
        let Some(shell) = self.shell() else {
            return;
        };
        let cover_size = self.fullscreen_player_cover_size();
        self.views
            .fullscreen_player
            .cover
            .set_square_size(cover_size);
        if presentation.current {
            shell.artwork.bind_playback_artwork_tile(
                &self.views.fullscreen_player.cover,
                presentation.artwork.clone(),
                cover_size,
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
        self.views.fullscreen_player.sync_meta_visible();
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
        let Some(shell) = self.shell() else {
            return;
        };
        shell
            .artwork
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

    fn animate_fullscreen_player(
        self: &Rc<Self>,
        opening: bool,
        from: Option<i32>,
        animated: bool,
    ) {
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
        if opening && from.is_none() {
            self.views.fullscreen_player.background.set_visible(false);
            root.remove_css_class("artwork-background");
        }
        let height = self.fullscreen_player_hidden_offset();
        let from_offset = f64::from(from.unwrap_or(if opening { height } else { 0 }));
        let to_offset = if opening { 0.0 } else { f64::from(height) };
        let duration_us = i64::from(if opening {
            FULLSCREEN_PLAYER_OPEN_TRANSITION_MS
        } else {
            FULLSCREEN_PLAYER_CLOSE_TRANSITION_MS
        }) * 1_000;
        let started_at = Rc::new(Cell::new(None));

        root.set_visible(true);
        root.set_opacity(1.0);
        root.set_can_target(opening);
        self.views
            .fullscreen_player
            .set_slide_offset(from_offset.round() as i32);

        let tick_shell = Rc::clone(self);
        let tick_started_at = Rc::clone(&started_at);
        let tick = root.add_tick_callback(move |root, clock| {
            let Some(app) = tick_shell.shell() else {
                return glib::ControlFlow::Break;
            };
            let now = clock.frame_time();
            let start = tick_started_at.get().unwrap_or_else(|| {
                tick_started_at.set(Some(now));
                now
            });
            let elapsed = now.saturating_sub(start);
            let progress = if animated && root.settings().is_gtk_enable_animations() {
                (elapsed as f64 / duration_us as f64).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let eased = 1.0 - (1.0 - progress).powi(3);
            let offset = from_offset + (to_offset - from_offset) * eased;
            tick_shell
                .views
                .fullscreen_player
                .set_slide_offset(offset.round() as i32);

            if progress >= 1.0 {
                root.set_can_target(opening);
                if opening {
                    tick_shell.views.fullscreen_player.set_slide_offset(0);
                    root.set_opacity(1.0);
                } else {
                    tick_shell.views.fullscreen_player.set_slide_offset(0);
                    root.set_opacity(0.0);
                    app.chrome
                        .window
                        .remove_css_class("fullscreen-artwork-bars");
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
                (self.shell().map_or(0, |shell| shell.chrome.window.height())
                    - BOTTOM_PLAYER_HEIGHT)
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
        ControlsView, CurrentMedia, CurrentMediaId, PlaybackOutput, QueueProvenance,
        QueueRepeatMode, QueueRow, QueueSummaryView, RunId, TrackReference, TransportStatus,
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
        current_change.transport.current = Some(current_media("Next", source));
        assert_eq!(
            fullscreen_playback_refresh(Some(&previous), &current_change),
            FullscreenPlaybackRefresh::Static
        );
        let presentation = NowPlayingPresentation::new(Some(&current_change));
        assert!(presentation.current);
        assert_eq!(presentation.title, "Next");
        assert_eq!(presentation.artist, "Artist");
        assert_eq!(presentation.album, "Album");
        assert_eq!(
            presentation.meta,
            ["2026", "FLAC · 24 bit · 96 kHz", "Local"]
        );
        let mut streaming = current_change.clone();
        let current = streaming.transport.current.as_mut().unwrap();
        current.stream = Some(playback::CurrentStream {
            local: false,
            format: Some("audio/mpeg".into()),
            transcoded: true,
            bitstream: false,
        });
        assert_eq!(
            NowPlayingPresentation::new(Some(&streaming)).meta,
            ["2026", "MP3", "Streaming"]
        );
        assert_eq!(
            rufin_core::settings::presentation::now_playing_labels(
                streaming.transport.current.as_ref(),
                Some("Living room")
            ),
            ["2026", "MP3", "Living room · Streaming"]
        );
        assert_eq!(
            fullscreen_playback_refresh(Some(&current_change), &streaming),
            FullscreenPlaybackRefresh::Static
        );
        let current = streaming.transport.current.as_mut().unwrap();
        current.stream = Some(playback::CurrentStream {
            local: true,
            format: Some("audio/ogg; codecs=opus".into()),
            transcoded: false,
            bitstream: false,
        });
        assert_eq!(
            NowPlayingPresentation::new(Some(&streaming)).meta,
            ["2026", "OPUS", "Local"]
        );
        let current = streaming.transport.current.as_mut().unwrap();
        Arc::make_mut(&mut current.row).track.metadata.source_format = Some("ogg".into());
        assert_eq!(
            NowPlayingPresentation::new(Some(&streaming)).meta,
            ["2026", "OPUS", "Local"]
        );
        let current = streaming.transport.current.as_mut().unwrap();
        let metadata = &mut Arc::make_mut(&mut current.row).track.metadata;
        metadata.source_format = Some("opus".into());
        metadata.audio_properties.bit_depth = None;
        metadata.audio_properties.sample_rate_hz = Some(48_000);
        current.stream.as_mut().unwrap().format = Some("audio/ogg".into());
        assert_eq!(
            NowPlayingPresentation::new(Some(&streaming)).meta,
            ["2026", "OPUS · 48 kHz", "Local"]
        );
    }

    fn current_media(title: &str, _source: library::SourceKey) -> CurrentMedia {
        let media_uri =
            library::source_entity_uri(&library::SourceId::new("source"), "track", title);
        CurrentMedia {
            stream: Some(playback::CurrentStream {
                local: true,
                format: Some("audio/flac".into()),
                transcoded: false,
                bitstream: false,
            }),
            id: CurrentMediaId {
                run: Some(RunId::new(1)),
                media_uri: media_uri.into(),
            },
            row: Arc::new(QueueRow {
                occurrence_id: None,
                position: 0,
                provenance: QueueProvenance::Manual,
                track: {
                    let reference = TrackReference {
                        media_uri: library::source_entity_uri(
                            &library::SourceId::new("source"),
                            "track",
                            title,
                        ),
                        artwork_binding: None,
                        metadata: library::TrackMetadata {
                            audio_properties: library::AudioProperties {
                                bit_depth: Some(24),
                                sample_rate_hz: Some(96_000),
                                ..Default::default()
                            },
                            title: title.to_string(),
                            artist: "Artist".to_string(),
                            album: "Album".to_string(),
                            album_display_artist: None,
                            duration_millis: 180_000,
                            disc_number: 0,
                            track_number: 0,
                            year: Some(2026),
                            release_date: None,
                            source_format: Some("audio/flac".to_string()),
                            musicbrainz_recording_id: None,
                            musicbrainz_release_track_id: None,
                            musicbrainz_album_id: None,
                            musicbrainz_release_group_id: None,
                            primary_artist_musicbrainz_id: None,
                            ..Default::default()
                        },
                    };
                    library::TrackView {
                        track_key: library::TrackKey::from_raw(1),
                        source_key: None,
                        source_id: None,
                        object_id: None,
                        media_uri: reference.media_uri,
                        metadata: reference.metadata,
                        artwork_binding: reference.artwork_binding,
                        state: Default::default(),
                        access: Default::default(),
                        links: Default::default(),
                    }
                },
            }),
        }
    }

    fn playback_view(
        _source: library::SourceKey,
        current: Option<CurrentMedia>,
        position_millis: u64,
    ) -> PlaybackView {
        PlaybackView {
            queue_loading: false,
            queue_window: Vec::new(),
            queue: QueueSummaryView {
                revision: 1,
                total: usize::from(current.is_some()),
                current_index: current.as_ref().map(|_| 0),
                next_index: None,
                can_next: false,
            },
            transport: TransportView {
                current,
                state: TransportStatus::Playing,
                desired_playing: true,
                position_millis,
                position_observed_at_millis: 0,
                playback_rate: 1.0,
                duration_millis: 180_000,
                can_seek: true,
                buffering_percent: None,
                error: None,
            },
            controls: ControlsView {
                repeat_mode: QueueRepeatMode::Off,
                shuffle_enabled: false,
                auto_dj_enabled: false,
                volume: 1.0,
                muted: false,
                audio_output: None,
                playback_output: PlaybackOutput::Local,
                sleep_timer: None,
            },
        }
    }
}
