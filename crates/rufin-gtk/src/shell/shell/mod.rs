mod activity;
pub(crate) mod build;
pub(crate) mod chrome;
mod diagnostics;
mod drop_import;
pub(crate) mod layout;
pub(crate) mod navigation;
mod topbar;
mod topbar_search;
use std::cell::Cell;
use std::rc::Rc;

use crate::shell::player::desktop::DesktopState;

use crate::preferences::preferences::source::SourceState;
use crate::widgets::artwork::ArtworkState;
use crate::widgets::downloads::DownloadsState;
use crate::widgets::feedback::ControlFeedbackState;
use crate::widgets::settings::SettingsState;
use chrome::WindowChrome;
use layout::ShellLayoutState;
use navigation::{NavigationState, NavigationWidgets};
use rufin_core::runtime::{DiagnosticsHandle, ProductHandles};
use startup::StartupState;

pub(crate) mod actions;
mod events;
pub(crate) mod route;
mod route_position;
pub(crate) mod selected_ui;
mod startup;
mod window_state;

use route::RouteViewport;
use selected_ui::SelectedUiState;

pub(crate) struct Shell {
    pub(crate) search_session: Rc<crate::library::SearchSession>,
    activity: std::cell::RefCell<Option<Rc<activity::ActivityView>>>,
    pub(crate) quitting: Rc<Cell<bool>>,
    pub(crate) storage_reset: crate::shell::application::StorageResetResult,
    pub(crate) diagnostics: DiagnosticsHandle,
    pub(crate) appearance: Rc<crate::preferences::appearance::ApplicationAppearance>,
    pub(crate) settings: Rc<SettingsState>,
    pub(crate) navigation: NavigationState,
    pub(crate) source: Rc<SourceState>,
    startup: StartupState,
    pub(crate) player_ui: Rc<crate::player::PlayerUi>,
    pub(crate) preferences: crate::preferences::preferences::PreferencesState,
    pub(crate) downloads: Rc<DownloadsState>,
    pub(crate) download_feedback: crate::shell::downloads::DownloadFeedbackState,
    pub(crate) control_feedback: Rc<ControlFeedbackState>,
    pub(crate) desktop: DesktopState,
    pub(crate) artwork: Rc<ArtworkState>,
    pub(crate) selected_ui: SelectedUiState,
    pub(crate) products: ProductHandles,
    pub(crate) web_controller: Rc<web::Controller>,
    pub(crate) chrome: WindowChrome,
    layout_state: ShellLayoutState,
    pub(crate) navigation_view: NavigationWidgets,
    pub(crate) route_viewport: RouteViewport,
}

mod full_artwork;
mod route_keyboard;

mod playlist_files;
