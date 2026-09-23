mod activity;
pub(crate) mod build;
pub(crate) mod chrome;
mod diagnostics;
pub(crate) mod layout;
pub(crate) mod navigation;
mod topbar;
mod topbar_search;
use std::cell::Cell;
use std::rc::Rc;

use crate::player::desktop::DesktopState;
use crate::player::right_panel::RightPanelWidgets;

use crate::preferences::source::SourceState;
use chrome::WindowChrome;
use gtk_widgets::artwork::ArtworkState;
use gtk_widgets::downloads::DownloadsState;
use gtk_widgets::feedback::ControlFeedbackState;
use gtk_widgets::settings::SettingsState;
use layout::ShellLayoutState;
use navigation::{NavigationState, NavigationWidgets};
use rufin_core::runtime::{DiagnosticsHandle, ProductHandles, SelectedSourceHandle};
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

impl Shell {
    pub(crate) fn selected_source_operations(&self) -> Option<SelectedSourceHandle> {
        self.selected_library()
            .as_deref()
            .map(|selected| selected.operations.clone())
    }
}

pub(crate) struct Shell {
    activity: std::cell::RefCell<Option<Rc<activity::ActivityView>>>,
    pub(crate) quitting: Rc<Cell<bool>>,
    pub(crate) diagnostics: DiagnosticsHandle,
    pub(crate) appearance: Rc<gtk_preferences::appearance::ApplicationAppearance>,
    pub(crate) settings: Rc<SettingsState>,
    pub(crate) navigation: NavigationState,
    pub(crate) source: Rc<SourceState>,
    startup: StartupState,
    pub(crate) media_menus: Rc<gtk_media_menus::media_menus::MediaMenus>,
    pub(crate) player_ui: Rc<gtk_player::PlayerUi>,
    pub(crate) preferences: Rc<gtk_preferences::Preferences>,
    pub(crate) downloads: Rc<DownloadsState>,
    pub(crate) download_feedback: crate::downloads::DownloadFeedbackState,
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
