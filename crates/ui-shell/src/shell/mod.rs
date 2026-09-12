pub(crate) mod build;
pub(crate) mod chrome;
mod diagnostics;
pub(crate) mod layout;
pub(crate) mod navigation;
use std::cell::Cell;
use std::rc::Rc;

use crate::player::desktop::DesktopState;
use crate::player::right_panel::RightPanelWidgets;
use crate::preferences::PreferencesState;
use crate::preferences::source::SourceState;
use chrome::WindowChrome;
use layout::ShellLayoutState;
use navigation::{NavigationState, NavigationWidgets};
use rufin_core::runtime::{DiagnosticsHandle, ProductHandles, SelectedSourceHandle};
use startup::StartupState;
use ui_shared::artwork::ArtworkState;
use ui_shared::downloads::DownloadsState;
use ui_shared::feedback::ControlFeedbackState;
use ui_shared::settings::SettingsState;

pub(crate) mod actions;
mod events;
pub(crate) mod route;
mod route_position;
pub(crate) mod selected_ui;
mod startup;
mod window_state;

use route::RouteViewport;
use selected_ui::SelectedUiState;

impl Shell {
    pub(crate) fn selected_source_operations(&self) -> Option<SelectedSourceHandle> {
        self.selected_library()
            .as_deref()
            .map(|selected| selected.operations.clone())
    }
}

pub(crate) struct Shell {
    pub(crate) quitting: Rc<Cell<bool>>,
    pub(crate) diagnostics: DiagnosticsHandle,
    pub(crate) appearance: crate::application::style::ApplicationAppearance,
    pub(crate) settings: Rc<SettingsState>,
    pub(crate) navigation: NavigationState,
    pub(crate) source: SourceState,
    startup: StartupState,
    pub(crate) media_menus: Rc<ui_shared::media_menus::MediaMenus>,
    pub(crate) player_ui: Rc<ui_player::PlayerUi>,
    pub(crate) preferences: PreferencesState,
    pub(crate) downloads: Rc<DownloadsState>,
    pub(crate) download_feedback: crate::downloads::DownloadFeedbackState,
    pub(crate) control_feedback: Rc<ControlFeedbackState>,
    pub(crate) desktop: DesktopState,
    pub(crate) artwork: Rc<ArtworkState>,
    pub(crate) selected_ui: SelectedUiState,
    pub(crate) products: ProductHandles,
    pub(crate) web_controller: rufin_core::api::Controller,
    pub(crate) chrome: WindowChrome,
    layout_state: ShellLayoutState,
    pub(crate) navigation_view: NavigationWidgets,
    pub(crate) route_viewport: RouteViewport,
    pub(crate) right_panel: RightPanelWidgets,
}

mod media_menus;

mod full_artwork;
mod route_keyboard;

mod catalog;
pub(crate) mod catalog_actions;

pub(crate) mod player_menus;
mod playlist_files;
pub(crate) mod playlist_picker;
