use std::rc::Rc;

use crate::RightSidebarMode;
use adw::prelude::*;

use crate::shell::Shell;
use crate::shell::layout::{ActiveLayoutProfile, resolve_layout};

pub(crate) struct RightPanelWidgets {
    pub(crate) right_split: gtk::Paned,
    pub(crate) right_panel_slot: gtk::ScrolledWindow,
    pub(crate) right_resize_handle: gtk::Box,
}

impl Shell {
    pub(crate) fn toggle_right_panel(self: &Rc<Self>) {
        let visible = self.right_sidebar_visible();
        self.set_right_sidebar_visible(!visible);
    }

    pub(crate) fn set_right_sidebar_visible(self: &Rc<Self>, visible: bool) {
        if !visible {
            self.player_ui.remember_queue_lyrics_open_position();
        }
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, self.layout_width()).profile;
        self.settings
            .update_app_settings("right sidebar setting", |settings| {
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

    pub(crate) fn toggle_lyrics_panel(self: &Rc<Self>) {
        let visible = !self.right_sidebar_visible() || !self.player_ui.lyrics.panel_visible.get();
        self.set_lyrics_panel_visible(visible);
    }

    pub(crate) fn set_lyrics_panel_visible(self: &Rc<Self>, visible: bool) {
        self.set_right_panel_media_visibility(
            visible,
            self.player_ui.right_panel.visualizer_visible.get(),
        );
    }

    pub(crate) fn set_visualizer_panel_visible(self: &Rc<Self>, visible: bool) {
        self.set_right_panel_media_visibility(self.player_ui.lyrics.panel_visible.get(), visible);
    }

    pub(crate) fn set_right_panel_media_visibility(
        self: &Rc<Self>,
        lyrics_visible: bool,
        visualizer_visible: bool,
    ) {
        if (lyrics_visible || visualizer_visible) && !self.right_sidebar_visible() {
            self.set_right_sidebar_visible(true);
        }
        self.player_ui
            .set_right_panel_media_visibility(lyrics_visible, visualizer_visible);
    }
}
