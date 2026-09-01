use std::cell::Cell;
use std::rc::Rc;

use crate::RightSidebarMode;
use adw::prelude::*;

use crate::shell::Shell;
use crate::shell::layout::{ActiveLayoutProfile, MIN_RESTORED_WINDOW_HEIGHT, resolve_layout};
use localization::tr;

use super::bottom::BOTTOM_PLAYER_HEIGHT;
const QUEUE_LYRICS_DEFAULT_LYRICS_HEIGHT: i32 = 300;
const QUEUE_LYRICS_HANDLE_HEIGHT: f64 = 10.0;

pub(crate) struct RightPanelWidgets {
    pub(crate) right_split: gtk::Paned,
    pub(crate) right_panel_slot: gtk::ScrolledWindow,
    pub(crate) right_resize_handle: gtk::Box,
    pub(crate) root: gtk::Box,
    pub(crate) queue_header_host: gtk::Box,
    pub(crate) queue_panel: gtk::Box,
    pub(crate) queue_search: gtk::SearchEntry,
    pub(crate) queue_clear_button: gtk::Button,
    pub(crate) queue_lyrics_split: gtk::Paned,
    pub(crate) lyrics_surface: gtk::Box,
    pub(crate) lyrics_host: gtk::Box,
    pub(crate) visualizer_visible: Cell<bool>,
}

pub(crate) struct RightPanelParts {
    pub(crate) root: gtk::Box,
    pub(crate) queue_header_host: gtk::Box,
    pub(crate) queue_panel: gtk::Box,
    pub(crate) queue_search: gtk::SearchEntry,
    pub(crate) queue_clear_button: gtk::Button,
    pub(crate) queue_lyrics_split: gtk::Paned,
    pub(crate) lyrics_surface: gtk::Box,
    pub(crate) lyrics_host: gtk::Box,
}

pub(crate) fn build_right_panel(
    end_window_controls: &impl IsA<gtk::Widget>,
    visualizer_area: &gtk::DrawingArea,
) -> RightPanelParts {
    let resource = crate::ui_resource::RIGHT_PANEL_RESOURCE;
    let builder = crate::ui_resource::builder(resource);
    crate::ui_resource::objects!(builder, resource, {
        root: gtk::Box,
        queue_lyrics_overlay: gtk::Overlay,
        queue_fixed_top: gtk::Box,
        queue_header_host: gtk::Box,
        queue_panel: gtk::Box,
        queue_search: gtk::SearchEntry,
        queue_clear_button: gtk::Button,
        window_controls_host: gtk::Box,
        queue_lyrics_split: gtk::Paned,
        lyrics_surface: gtk::Box,
        media_overlay: gtk::Overlay,
        lyrics_host: gtk::Box,
    });
    window_controls_host.append(end_window_controls);
    media_overlay.add_overlay(visualizer_area);
    media_overlay.set_measure_overlay(visualizer_area, false);
    media_overlay.add_overlay(&lyrics_host);
    media_overlay.set_measure_overlay(&lyrics_host, false);
    queue_lyrics_overlay.remove_overlay(&queue_panel);
    queue_lyrics_overlay.add_overlay(&queue_panel);
    queue_lyrics_overlay.set_measure_overlay(&queue_panel, false);
    queue_lyrics_overlay.set_measure_overlay(&queue_lyrics_split, false);
    let positioned_queue = queue_panel.clone();
    let positioned_top = queue_fixed_top.clone();
    let positioned_split = queue_lyrics_split.clone();
    queue_lyrics_overlay.connect_get_child_position(move |overlay, child| {
        if child != positioned_queue.upcast_ref::<gtk::Widget>() {
            return None;
        }
        let top = positioned_top.height().max(0);
        Some(gtk::gdk::Rectangle::new(
            0,
            top,
            overlay.width().max(0),
            positioned_split.position().saturating_sub(top).max(0),
        ))
    });
    let allocate_queue = queue_lyrics_overlay.clone();
    queue_lyrics_split.connect_position_notify(move |_| allocate_queue.queue_allocate());
    root.append(&queue_lyrics_overlay);

    RightPanelParts {
        root,
        queue_header_host,
        queue_panel,
        queue_search,
        queue_clear_button,
        queue_lyrics_split,
        lyrics_surface,
        lyrics_host,
    }
}

impl Shell {
    fn save_queue_lyrics_height(&self) {
        if !self.lyrics.panel_visible.get() && !self.right_panel.visualizer_visible.get() {
            return;
        }
        let height = self
            .right_panel
            .queue_lyrics_split
            .height()
            .saturating_sub(self.right_panel.queue_lyrics_split.position());
        if height <= 0 {
            return;
        }
        self.set_app_setting("queue lyrics height", Some(height), |settings| {
            &mut settings.queue_lyrics_height
        });
    }

    pub(crate) fn remember_queue_lyrics_open_position(&self) {
        self.save_queue_lyrics_height();
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

pub(crate) fn connect_queue_lyrics_split(shell: &Rc<Shell>) {
    let saved_height = shell.settings.current.borrow().queue_lyrics_height;
    let available_height = queue_lyrics_restore_available_height(shell);
    shell
        .right_panel
        .queue_lyrics_split
        .set_position(queue_lyrics_initial_position(
            available_height,
            saved_height,
        ));
    let drag = gtk::GestureDrag::new();
    drag.set_button(1);
    drag.set_propagation_phase(gtk::PropagationPhase::Bubble);
    let resizing = Rc::new(Cell::new(false));
    let resize_split = shell.right_panel.queue_lyrics_split.clone();
    let resize_active = Rc::clone(&resizing);
    drag.connect_drag_begin(move |_, _, start_y| {
        resize_active.set(
            (start_y - f64::from(resize_split.position())).abs()
                <= QUEUE_LYRICS_HANDLE_HEIGHT / 2.0,
        );
    });
    let drag_shell = Rc::clone(shell);
    let resize_active = Rc::clone(&resizing);
    drag.connect_drag_end(move |_, _, _| {
        if resize_active.replace(false) {
            drag_shell.save_queue_lyrics_height();
        }
    });
    shell.right_panel.queue_lyrics_split.add_controller(drag);
}

pub(crate) fn apply_sidebar_media_visibility(shell: Rc<Shell>) {
    let lyrics_visible = shell.lyrics.panel_visible.get();
    let visualizer_visible = shell.right_panel.visualizer_visible.get();
    let visible = lyrics_visible || visualizer_visible;
    shell.right_panel.lyrics_surface.set_visible(visible);
    shell.right_panel.lyrics_host.set_visible(lyrics_visible);
    shell
        .player_view
        .visualizer
        .sidebar_area
        .set_visible(visualizer_visible);
    shell
        .player_view
        .visualizer
        .sidebar_area
        .set_opacity(if lyrics_visible { 0.48 } else { 1.0 });
    if visible {
        let available_height = queue_lyrics_restore_available_height(&shell);
        let saved_height = shell.settings.current.borrow().queue_lyrics_height;
        shell
            .right_panel
            .queue_lyrics_split
            .set_position(queue_lyrics_initial_position(
                available_height,
                saved_height,
            ));
    }
    if lyrics_visible {
        shell.sync_visible_lyrics_surfaces();
    } else {
        shell.update_lyrics_highlight();
    }
    shell.sync_visualizer_state();
}

fn queue_lyrics_restore_available_height(shell: &Shell) -> i32 {
    let split_height = shell.right_panel.queue_lyrics_split.height();
    if split_height > 1 {
        return split_height;
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

fn queue_lyrics_initial_position(available_height: i32, saved_height: Option<i32>) -> i32 {
    let lyrics_height = saved_height
        .filter(|height| *height > 0)
        .unwrap_or(QUEUE_LYRICS_DEFAULT_LYRICS_HEIGHT)
        .clamp(0, available_height.max(0));
    available_height.saturating_sub(lyrics_height)
}
