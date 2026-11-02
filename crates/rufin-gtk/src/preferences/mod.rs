//! Preferences, source setup, Connect and desktop appearance presentation.
pub mod appearance;
mod download_quality;
pub mod preferences;

use rufin_core::settings::*;
use std::rc::Rc;

pub struct Preferences {
    pub settings: Rc<crate::widgets::settings::SettingsState>,
    pub source: Rc<preferences::source::SourceState>,
    pub state: preferences::PreferencesState,
    pub products: rufin_core::runtime::ProductHandles,
    pub player_ui: Rc<crate::player::PlayerUi>,
    pub downloads: Rc<crate::widgets::downloads::DownloadsState>,
    pub artwork: Rc<crate::widgets::artwork::ArtworkState>,
    pub appearance: Rc<appearance::ApplicationAppearance>,
    pub control_feedback: Rc<crate::widgets::feedback::ControlFeedbackState>,
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
    #[cfg(not(target_os = "macos"))]
    TrayEnabled(bool),
    #[cfg(not(target_os = "macos"))]
    KeepRunningAfterClose(bool),
    #[cfg(not(target_os = "macos"))]
    StartMinimized(bool),
    LyricsPanelVisible(bool),
    VisualizerPanelVisible(bool),
    Quit(&'static str),
    ResetStorage(Box<rufin_core::settings::StorageResetRequest>),
    ExportActivity(
        Option<library::ActivityCsvFormat>,
        Option<sources::SourceId>,
    ),
}
