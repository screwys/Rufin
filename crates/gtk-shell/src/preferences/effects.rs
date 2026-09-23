use crate::shell::Shell;
use gtk_preferences::Effect;
use std::rc::Rc;

impl Shell {
    pub(crate) fn apply_preferences_effect(self: &Rc<Self>, effect: Effect) {
        match effect {
            Effect::LayoutChanged => self.update_layout(),
            Effect::SidebarChanged => self.rebuild_sidebar_navigation(),
            Effect::CatalogChanged => self.reconcile_mounted_route(),
            Effect::ReloadCatalog => self.render_current_route(),
            Effect::RefreshHome => self.refresh_mounted_home(),
            Effect::MediaControlsChanged => self.update_media_controls(),
            Effect::PrivateModeChanged => self.refresh_tray_private_mode(),
            Effect::WithdrawNotification => self.withdraw_now_playing_notification(),
            Effect::PlaylistPickerChanged => {
                crate::shell::playlist_picker::refresh_context_playlist_picker(self)
            }
            #[cfg(not(target_os = "macos"))]
            Effect::TrayEnabled(enabled) => self.set_tray_enabled(enabled),
            #[cfg(not(target_os = "macos"))]
            Effect::KeepRunningAfterClose(enabled) => self.set_keep_running_after_close(enabled),
            #[cfg(not(target_os = "macos"))]
            Effect::StartMinimized(enabled) => self.set_start_minimized_enabled(enabled),
            Effect::LyricsPanelVisible(enabled) => self.set_lyrics_panel_visible(enabled),
            Effect::VisualizerPanelVisible(enabled) => self.set_visualizer_panel_visible(enabled),
            Effect::Quit(reason) => self.request_quit(reason),
            Effect::ExportActivity(format, source) => self.export_activity_dialog(format, source),
        }
    }
}
