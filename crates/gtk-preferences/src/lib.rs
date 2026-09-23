//! Preferences, source setup, Connect and desktop appearance presentation.
pub mod appearance;
mod download_presentation;
pub mod preferences;
pub mod ui_resource;

use rufin_core::settings::*;
use std::rc::Rc;

pub struct Preferences {
    pub settings: Rc<gtk_widgets::settings::SettingsState>,
    pub source: Rc<preferences::source::SourceState>,
    pub state: preferences::PreferencesState,
    pub products: rufin_core::runtime::ProductHandles,
    pub player_ui: Rc<gtk_player::PlayerUi>,
    pub downloads: Rc<gtk_widgets::downloads::DownloadsState>,
    pub artwork: Rc<gtk_widgets::artwork::ArtworkState>,
    pub appearance: Rc<appearance::ApplicationAppearance>,
    pub control_feedback: Rc<gtk_widgets::feedback::ControlFeedbackState>,
    pub web_controller: Rc<web::Controller>,
    pub window: gtk::ApplicationWindow,
    pub toast_overlay: adw::ToastOverlay,
    pub app_root_overlay: gtk::Overlay,
    pub effects: Box<dyn Fn(Effect)>,
}

/// Work owned by the mounted desktop window after a preference changes.
pub enum Effect {
    LayoutChanged,
    SidebarChanged,
    CatalogChanged,
    ReloadCatalog,
    RefreshHome,
    MediaControlsChanged,
    PrivateModeChanged,
    WithdrawNotification,
    PlaylistPickerChanged,
    #[cfg(not(target_os = "macos"))]
    TrayEnabled(bool),
    #[cfg(not(target_os = "macos"))]
    KeepRunningAfterClose(bool),
    #[cfg(not(target_os = "macos"))]
    StartMinimized(bool),
    LyricsPanelVisible(bool),
    VisualizerPanelVisible(bool),
    Quit(&'static str),
    ExportActivity(
        Option<library::ActivityCsvFormat>,
        Option<sources::SourceId>,
    ),
}

pub fn register_resources() -> Result<(), String> {
    gtk_widgets::register_resources()?;
    gtk_player::register_resources()?;
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gtk::gio::resources_register_include!("gtk-preferences.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}
