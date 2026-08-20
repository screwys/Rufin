use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::RightSidebarMode;
use adw::prelude::*;

use crate::layout::allocation_owner;
use crate::shell::Shell;
use crate::shell::actions::icon_button;
use crate::shell::layout::{
    ActiveLayoutProfile, MIN_RESTORED_WINDOW_HEIGHT, WINDOW_CHROME_MARGIN_END, resolve_layout,
};
use localization::tr;

use super::bottom::BOTTOM_PLAYER_HEIGHT;
use super::fullscreen::build_visualizer_area;
const QUEUE_LYRICS_DEFAULT_LYRICS_HEIGHT: i32 = 300;
const QUEUE_LYRICS_RESIZE_HANDLE_HEIGHT: i32 = 10;
const QUEUE_HEADER_TOP_MARGIN: i32 = 10;

pub(crate) struct RightPanelWidgets {
    pub(crate) right_split: gtk::Paned,
    pub(crate) right_panel_slot: gtk::ScrolledWindow,
    pub(crate) right_resize_handle: gtk::Box,
    pub(crate) root: gtk::Box,
    pub(crate) queue_panel: gtk::Box,
    pub(crate) queue_search: gtk::SearchEntry,
    pub(crate) queue_clear_button: gtk::Button,
    pub(crate) queue_lyrics_overlay: gtk::Overlay,
    pub(crate) lyrics_surface: gtk::Box,
    pub(crate) lyrics_resize_handle: gtk::Box,
    pub(crate) lyrics_host: gtk::Box,
    pub(crate) visualizer_area: gtk::DrawingArea,
    pub(crate) visualizer_visible: Cell<bool>,
}

pub(crate) struct RightPanelParts {
    pub(crate) root: gtk::Box,
    pub(crate) queue_panel: gtk::Box,
    pub(crate) queue_search: gtk::SearchEntry,
    pub(crate) queue_clear_button: gtk::Button,
    pub(crate) queue_lyrics_overlay: gtk::Overlay,
    pub(crate) lyrics_surface: gtk::Box,
    pub(crate) lyrics_resize_handle: gtk::Box,
    pub(crate) lyrics_host: gtk::Box,
    pub(crate) visualizer_area: gtk::DrawingArea,
    pub(crate) visualizer_levels: Rc<RefCell<Vec<f64>>>,
}

pub(crate) fn build_right_panel(end_window_controls: &impl IsA<gtk::Widget>) -> RightPanelParts {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("right-panel");
    root.set_hexpand(true);
    root.set_vexpand(true);

    let queue_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    queue_header.add_css_class("sidebar-header");
    queue_header.add_css_class("queue-toolbar");
    queue_header.set_valign(gtk::Align::Center);
    queue_header.set_margin_top(QUEUE_HEADER_TOP_MARGIN);
    queue_header.set_margin_bottom(0);
    queue_header.set_margin_start(QUEUE_HEADER_TOP_MARGIN);
    queue_header.set_margin_end(WINDOW_CHROME_MARGIN_END);

    let queue_search = gtk::SearchEntry::new();
    queue_search.add_css_class("queue-search");
    let search_label = tr("Search queue");
    queue_search.update_property(&[gtk::accessible::Property::Label(&search_label)]);
    queue_search.set_hexpand(true);
    queue_search.set_height_request(30);
    queue_header.append(&queue_search);

    let queue_clear_button = icon_button("edit-clear-bundled-symbolic", "Clear queue");
    queue_header.append(&queue_clear_button);
    queue_header.append(end_window_controls);
    let queue_panel = gtk::Box::new(gtk::Orientation::Vertical, 6);
    queue_panel.add_css_class("queue-panel");
    queue_panel.set_vexpand(true);
    queue_panel.set_margin_top(8);
    queue_panel.set_margin_start(QUEUE_HEADER_TOP_MARGIN);
    queue_panel.set_margin_end(0);
    queue_panel.set_margin_bottom(0);
    queue_panel.set_overflow(gtk::Overflow::Hidden);

    let queue_region = gtk::Box::new(gtk::Orientation::Vertical, 0);
    queue_region.set_vexpand(true);
    queue_region.append(&queue_header);
    queue_region.append(&queue_panel);

    let lyrics_host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    lyrics_host.set_hexpand(true);
    lyrics_host.set_vexpand(true);
    let visualizer_levels = Rc::new(RefCell::new(Vec::new()));
    let visualizer_area = build_visualizer_area(Rc::clone(&visualizer_levels));
    visualizer_area.remove_css_class("fullscreen-player-visualizer-area");
    visualizer_area.add_css_class("sidebar-visualizer-area");
    let media_background = gtk::Box::new(gtk::Orientation::Vertical, 0);
    media_background.set_hexpand(true);
    media_background.set_vexpand(true);
    let media_overlay = gtk::Overlay::new();
    media_overlay.set_hexpand(true);
    media_overlay.set_vexpand(true);
    media_overlay.set_child(Some(&media_background));
    media_overlay.add_overlay(&visualizer_area);
    media_overlay.set_measure_overlay(&visualizer_area, false);
    media_overlay.add_overlay(&lyrics_host);
    media_overlay.set_measure_overlay(&lyrics_host, false);
    let lyrics_resize_handle = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    lyrics_resize_handle.add_css_class("queue-lyrics-resize-handle");
    lyrics_resize_handle.set_height_request(QUEUE_LYRICS_RESIZE_HANDLE_HEIGHT);
    lyrics_resize_handle.set_hexpand(true);
    lyrics_resize_handle.set_focusable(false);
    lyrics_resize_handle.set_cursor_from_name(Some("row-resize"));
    lyrics_resize_handle.set_accessible_role(gtk::AccessibleRole::Separator);
    let resize_label = tr("Hold and drag to resize");
    lyrics_resize_handle.update_property(&[gtk::accessible::Property::Label(&resize_label)]);

    let lyrics_surface = gtk::Box::new(gtk::Orientation::Vertical, 0);
    lyrics_surface.add_css_class("queue-lyrics-surface");
    lyrics_surface.set_overflow(gtk::Overflow::Hidden);
    lyrics_surface.set_hexpand(true);
    lyrics_surface.set_halign(gtk::Align::Fill);
    lyrics_surface.set_valign(gtk::Align::End);
    lyrics_surface.append(&lyrics_resize_handle);
    lyrics_surface.append(&media_overlay);

    let queue_lyrics_overlay = gtk::Overlay::new();
    queue_lyrics_overlay.add_css_class("queue-lyrics-overlay");
    queue_lyrics_overlay.set_hexpand(true);
    queue_lyrics_overlay.set_vexpand(true);
    queue_lyrics_overlay.set_child(Some(&queue_region));
    queue_lyrics_overlay.add_overlay(&lyrics_surface);
    queue_lyrics_overlay.set_measure_overlay(&lyrics_surface, false);
    let sized_surface = lyrics_surface.clone();
    let queue_lyrics_owner = allocation_owner(&queue_lyrics_overlay, move |_, height| {
        if height >= QUEUE_LYRICS_RESIZE_HANDLE_HEIGHT && sized_surface.height_request() > height {
            sized_surface.set_height_request(height);
        }
    });
    root.append(&queue_lyrics_owner);

    RightPanelParts {
        root,
        queue_panel,
        queue_search,
        queue_clear_button,
        queue_lyrics_overlay,
        lyrics_surface,
        lyrics_resize_handle,
        lyrics_host,
        visualizer_area,
        visualizer_levels,
    }
}

impl Shell {
    fn save_queue_lyrics_height(&self, height: i32) {
        if !self.lyrics.panel_visible.get() && !self.right_panel.visualizer_visible.get() {
            return;
        }
        if height < QUEUE_LYRICS_RESIZE_HANDLE_HEIGHT {
            return;
        }
        self.update_app_settings("queue lyrics height", |settings| {
            if settings.queue_lyrics_height == Some(height) {
                return false;
            }
            settings.queue_lyrics_height = Some(height);
            true
        });
    }

    pub(crate) fn remember_queue_lyrics_open_position(&self) {
        if !self.lyrics.panel_visible.get() && !self.right_panel.visualizer_visible.get() {
            return;
        }
        self.save_queue_lyrics_height(self.right_panel.lyrics_surface.height());
    }

    pub(crate) fn toggle_right_panel(self: &Rc<Self>) {
        let visible = self.right_sidebar_visible();
        self.set_right_sidebar_visible(!visible);
    }

    pub(crate) fn set_right_sidebar_visible(self: &Rc<Self>, visible: bool) {
        if !visible {
            self.remember_queue_lyrics_open_position();
        }
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, self.layout_width()).profile;
        self.update_app_settings("right sidebar setting", |settings| {
            let profile = match active_profile {
                ActiveLayoutProfile::Default => &mut settings.layout.default_profile,
                ActiveLayoutProfile::Narrow => &mut settings.layout.narrow_profile,
            };
            if visible {
                if profile.right_sidebar.is_visible() {
                    return false;
                }
                profile.right_sidebar = RightSidebarMode::Visible;
            } else {
                if !profile.right_sidebar.is_visible() {
                    return false;
                }
                profile.right_sidebar = RightSidebarMode::Hidden;
            }
            settings.layout.sanitize();
            true
        });
        self.update_layout();
        self.chrome.window.queue_resize();
    }

    pub(crate) fn update_right_panel_button(&self) {
        let visible = self.right_sidebar_visible();
        let label = if visible {
            tr("Hide sidebar")
        } else {
            tr("Show sidebar")
        };
        super::icons::set_queue_sidebar_icon(&self.player_view.player_controls.queue_icon, visible);
        self.player_view
            .player_controls
            .queue_button
            .set_tooltip_text(Some(&label));
        self.player_view
            .player_controls
            .queue_button
            .update_property(&[gtk::accessible::Property::Label(&label)]);
    }

    pub(crate) fn toggle_lyrics_panel(self: &Rc<Self>) {
        let visible = !self.right_sidebar_visible() || !self.lyrics.panel_visible.get();
        self.set_lyrics_panel_visible(visible);
    }

    pub(crate) fn set_lyrics_panel_visible(self: &Rc<Self>, visible: bool) {
        self.set_right_panel_media_visibility(visible, self.right_panel.visualizer_visible.get());
    }

    pub(crate) fn set_visualizer_panel_visible(self: &Rc<Self>, visible: bool) {
        self.set_right_panel_media_visibility(self.lyrics.panel_visible.get(), visible);
    }

    pub(crate) fn set_right_panel_media_visibility(
        self: &Rc<Self>,
        lyrics_visible: bool,
        visualizer_visible: bool,
    ) {
        if (lyrics_visible || visualizer_visible) && !self.right_sidebar_visible() {
            self.set_right_sidebar_visible(true);
        }
        if (!lyrics_visible && self.lyrics.panel_visible.get())
            || (!visualizer_visible && self.right_panel.visualizer_visible.get())
        {
            self.remember_queue_lyrics_open_position();
        }

        let lyrics_changed = self.lyrics.panel_visible.replace(lyrics_visible) != lyrics_visible;
        let visualizer_changed = self
            .right_panel
            .visualizer_visible
            .replace(visualizer_visible)
            != visualizer_visible;
        if lyrics_changed || visualizer_changed {
            self.update_app_settings("right panel media visibility", |settings| {
                if settings.lyrics_panel_visible == lyrics_visible
                    && settings.visualizer_panel_visible == visualizer_visible
                {
                    return false;
                }
                settings.lyrics_panel_visible = lyrics_visible;
                settings.visualizer_panel_visible = visualizer_visible;
                true
            });
        }
        apply_sidebar_media_visibility(Rc::clone(self));
    }
}

pub(crate) fn connect_queue_lyrics_overlay(shell: &Rc<Shell>) {
    let saved_height = shell.settings.current.borrow().queue_lyrics_height;
    let available_height = queue_lyrics_restore_available_height(shell);
    shell
        .right_panel
        .lyrics_surface
        .set_height_request(queue_lyrics_initial_height(available_height, saved_height));
    let dragging = Rc::new(std::cell::Cell::new(false));
    let resize_shell = Rc::clone(shell);
    let resize_dragging = Rc::clone(&dragging);
    shell
        .right_panel
        .lyrics_surface
        .connect_height_request_notify(move |_| {
            if !resize_dragging.get() {
                resize_shell.schedule_queue_panel_render();
            }
        });

    let start_height = Rc::new(std::cell::Cell::new(None));
    let drag = gtk::GestureDrag::new();
    drag.set_button(1);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    let drag_shell = Rc::clone(shell);
    let drag_start_height = Rc::clone(&start_height);
    let drag_active = Rc::clone(&dragging);
    drag.connect_drag_begin(move |gesture, _, start_y| {
        drag_active.set(false);
        drag_start_height.set(None);
        let overlay = &drag_shell.right_panel.queue_lyrics_overlay;
        let surface = &drag_shell.right_panel.lyrics_surface;
        let handle = &drag_shell.right_panel.lyrics_resize_handle;
        let surface_height = surface.height();
        let handle_top = overlay.height().saturating_sub(surface_height);
        let handle_bottom = handle_top.saturating_add(handle.height().max(1));
        if !handle.is_mapped()
            || !surface.is_visible()
            || start_y < f64::from(handle_top)
            || start_y > f64::from(handle_bottom)
        {
            gesture.set_state(gtk::EventSequenceState::Denied);
            return;
        }

        gesture.set_state(gtk::EventSequenceState::Claimed);
        drag_active.set(true);
        drag_start_height.set(Some(surface_height));
    });
    let drag_shell = Rc::clone(shell);
    let drag_start_height = Rc::clone(&start_height);
    drag.connect_drag_update(move |_, _, offset_y| {
        let Some(start_height) = drag_start_height.get() else {
            return;
        };
        let available_height = drag_shell.right_panel.queue_lyrics_overlay.height();
        let requested = start_height.saturating_sub(offset_y.round() as i32);
        drag_shell
            .right_panel
            .lyrics_surface
            .set_height_request(queue_lyrics_clamped_height(available_height, requested));
    });
    let drag_shell = Rc::clone(shell);
    let drag_start_height = Rc::clone(&start_height);
    let drag_active = Rc::clone(&dragging);
    drag.connect_drag_end(move |_, _, _| {
        drag_active.set(false);
        if drag_start_height.take().is_none() {
            return;
        }
        drag_shell.save_queue_lyrics_height(drag_shell.right_panel.lyrics_surface.height_request());
        drag_shell.schedule_queue_panel_render();
    });
    shell.right_panel.queue_lyrics_overlay.add_controller(drag);
}

pub(crate) fn apply_sidebar_media_visibility(shell: Rc<Shell>) {
    let lyrics_visible = shell.lyrics.panel_visible.get();
    let visualizer_visible = shell.right_panel.visualizer_visible.get();
    let visible = lyrics_visible || visualizer_visible;
    shell.right_panel.lyrics_surface.set_visible(visible);
    shell.right_panel.lyrics_host.set_visible(lyrics_visible);
    shell
        .right_panel
        .visualizer_area
        .set_visible(visualizer_visible);
    shell
        .right_panel
        .visualizer_area
        .set_opacity(if lyrics_visible { 0.32 } else { 1.0 });
    shell.right_panel.lyrics_resize_handle.set_visible(visible);
    shell.right_panel.lyrics_surface.set_valign(gtk::Align::End);
    shell.right_panel.lyrics_surface.set_vexpand(false);
    if visible {
        let available_height = queue_lyrics_restore_available_height(&shell);
        let saved_height = shell.settings.current.borrow().queue_lyrics_height;
        shell
            .right_panel
            .lyrics_surface
            .set_height_request(queue_lyrics_initial_height(available_height, saved_height));
    }
    shell.schedule_queue_panel_render();
    if lyrics_visible {
        shell.sync_visible_lyrics_surfaces();
    } else {
        shell.update_lyrics_highlight();
    }
    shell.sync_visualizer_state();
}

fn queue_lyrics_restore_available_height(shell: &Shell) -> i32 {
    let overlay_height = shell.right_panel.queue_lyrics_overlay.height();
    if overlay_height > 1 {
        return overlay_height;
    }
    let window_height = shell.chrome.window.height();
    if window_height > MIN_RESTORED_WINDOW_HEIGHT {
        return queue_lyrics_estimated_split_height(window_height);
    }
    if let Some(window_height) = shell.settings.current.borrow().window_height
        && window_height > MIN_RESTORED_WINDOW_HEIGHT
    {
        return queue_lyrics_estimated_split_height(window_height);
    }
    queue_lyrics_estimated_split_height(900)
}

fn queue_lyrics_estimated_split_height(window_height: i32) -> i32 {
    (window_height - BOTTOM_PLAYER_HEIGHT - 48).max(1)
}

fn queue_lyrics_clamped_height(available_height: i32, height: i32) -> i32 {
    if available_height <= QUEUE_LYRICS_RESIZE_HANDLE_HEIGHT {
        return available_height.max(0);
    }
    height.clamp(QUEUE_LYRICS_RESIZE_HANDLE_HEIGHT, available_height)
}

fn queue_lyrics_initial_height(available_height: i32, saved_height: Option<i32>) -> i32 {
    saved_height
        .filter(|height| *height > 0)
        .map(|height| queue_lyrics_clamped_height(available_height, height))
        .unwrap_or_else(|| {
            queue_lyrics_clamped_height(available_height, QUEUE_LYRICS_DEFAULT_LYRICS_HEIGHT)
        })
}
