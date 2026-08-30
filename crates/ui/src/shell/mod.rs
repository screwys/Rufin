pub(crate) mod build;
pub(crate) mod chrome;
mod diagnostics;
pub(crate) mod layout;
pub(crate) mod navigation;
use std::cell::Cell;
use std::rc::Rc;

use crate::downloads::DownloadsState;
use crate::player::PlayerDesktopWidgets;
use crate::player::desktop::DesktopState;
use crate::player::lyrics::state::LyricsState;
use crate::player::right_panel::RightPanelWidgets;
use crate::player::state::PlaybackState;
use crate::preferences::PreferencesState;
use crate::preferences::source::SourceState;
use crate::runtime::{DiagnosticsHandle, ProductHandles, SelectedSourceHandle};
use crate::settings::SettingsState;
use actions::ControlFeedbackState;
use chrome::WindowChrome;
use cover::ArtworkState;
use layout::ShellLayoutState;
use localization::LocalizationState;
use navigation::{NavigationState, NavigationWidgets};
use startup::StartupState;

pub(crate) mod actions;
pub(crate) mod cover;
mod events;
mod localization;
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
    pub(crate) home_showcase_variation: Cell<i64>,
    pub(crate) home_explore_variation: Cell<i64>,
    pub(crate) diagnostics: DiagnosticsHandle,
    pub(crate) appearance: crate::application::style::ApplicationAppearance,
    pub(crate) settings: SettingsState,
    pub(crate) navigation: NavigationState,
    pub(crate) source: SourceState,
    startup: StartupState,
    pub(crate) playback: PlaybackState,
    pub(crate) lyrics: LyricsState,
    pub(crate) preferences: PreferencesState,
    pub(crate) downloads: DownloadsState,
    pub(crate) control_feedback: ControlFeedbackState,
    localization: LocalizationState,
    pub(crate) desktop: DesktopState,
    artwork: ArtworkState,
    pub(crate) selected_ui: SelectedUiState,
    pub(crate) products: ProductHandles,
    pub(crate) chrome: WindowChrome,
    layout_state: ShellLayoutState,
    pub(crate) navigation_view: NavigationWidgets,
    pub(crate) route_viewport: RouteViewport,
    pub(crate) right_panel: RightPanelWidgets,
    pub(crate) player_view: PlayerDesktopWidgets,
}
