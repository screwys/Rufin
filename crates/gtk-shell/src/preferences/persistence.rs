use std::rc::Rc;

use crate::{LeftSidebarMode, RightSidebarMode};
use adw::prelude::*;

use crate::shell::Shell;
use crate::shell::layout::{ActiveLayoutProfile, ResolvedLeftSidebarMode, resolve_layout};

impl Shell {
    pub(crate) fn toggle_active_left_sidebar_size(self: &Rc<Self>) {
        let next_mode = if self.left_sidebar_mode() == ResolvedLeftSidebarMode::Full {
            LeftSidebarMode::Compact
        } else {
            LeftSidebarMode::Full
        };
        self.set_active_left_sidebar_mode(next_mode);
    }

    pub(crate) fn set_active_left_sidebar_mode(self: &Rc<Self>, mode: LeftSidebarMode) {
        let window_width = self.layout_width();
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, window_width).profile;
        if self
            .settings
            .update_app_settings("left sidebar setting", |settings| {
                let mut requested = settings.layout.clone();
                let profile = match active_profile {
                    ActiveLayoutProfile::Default => &mut requested.default_profile,
                    ActiveLayoutProfile::Narrow => &mut requested.narrow_profile,
                };
                profile.left_sidebar = mode;
                if mode == LeftSidebarMode::Full
                    && resolve_layout(&requested, window_width).left_sidebar
                        != ResolvedLeftSidebarMode::Full
                {
                    let profile = match active_profile {
                        ActiveLayoutProfile::Default => &mut requested.default_profile,
                        ActiveLayoutProfile::Narrow => &mut requested.narrow_profile,
                    };
                    profile.right_sidebar = RightSidebarMode::Hidden;
                }
                requested.sanitize();
                if settings.layout == requested {
                    return false;
                }
                settings.layout = requested;
                true
            })
            .is_none()
        {
            return;
        }
        self.update_layout();
        self.chrome.window.queue_resize();
    }

    pub(crate) fn save_left_sidebar_drag(
        self: &Rc<Self>,
        mode: LeftSidebarMode,
        width: i32,
        hide_right: bool,
    ) {
        let active_profile =
            resolve_layout(&self.settings.current.borrow().layout, self.layout_width()).profile;
        self.settings
            .update_app_settings("left sidebar drag", |settings| {
                let profile = match active_profile {
                    ActiveLayoutProfile::Default => &mut settings.layout.default_profile,
                    ActiveLayoutProfile::Narrow => &mut settings.layout.narrow_profile,
                };
                let mut changed = false;
                if profile.left_sidebar != mode {
                    profile.left_sidebar = mode;
                    changed = true;
                }
                if hide_right && profile.right_sidebar != RightSidebarMode::Hidden {
                    profile.right_sidebar = RightSidebarMode::Hidden;
                    changed = true;
                }
                if mode == LeftSidebarMode::Full {
                    let width =
                        width.clamp(crate::MIN_LEFT_SIDEBAR_WIDTH, crate::MAX_LEFT_SIDEBAR_WIDTH);
                    if settings.layout.preferred_left_sidebar_width != width {
                        settings.layout.preferred_left_sidebar_width = width;
                        changed = true;
                    }
                }
                changed
            });
        self.update_layout();
    }

    pub(crate) fn save_preferred_right_sidebar_width(&self, width: i32) {
        self.settings
            .update_app_settings("right sidebar width", |settings| {
                let width = width.clamp(
                    crate::MIN_RIGHT_SIDEBAR_WIDTH,
                    crate::MAX_RIGHT_SIDEBAR_WIDTH,
                );
                if settings.layout.preferred_right_sidebar_width == width {
                    return false;
                }
                settings.layout.preferred_right_sidebar_width = width;
                true
            });
    }
}
