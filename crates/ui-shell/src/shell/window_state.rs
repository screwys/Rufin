use super::Shell;
use crate::{DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, sanitized_window_size};
use adw::prelude::*;

pub(crate) fn initial_window_size(width: Option<i32>, height: Option<i32>) -> (i32, i32) {
    sanitized_window_size(width, height).unwrap_or((DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT))
}

impl Shell {
    pub(crate) fn save_window_state(&self) {
        self.player_ui.remember_queue_lyrics_open_position();
        if self.chrome.window.is_maximized() || self.chrome.window.is_fullscreen() {
            return;
        }
        let Some((width, height)) = sanitized_window_size(
            Some(self.chrome.window.width()),
            Some(self.chrome.window.height()),
        ) else {
            return;
        };

        self.settings
            .update_app_settings("window state", |settings| {
                if settings.window_width == Some(width) && settings.window_height == Some(height) {
                    return false;
                }
                settings.window_width = Some(width);
                settings.window_height = Some(height);
                true
            });
    }
}
