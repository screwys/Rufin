//! Persistent playback, queue and lyrics presentation.
use playback::PlaybackSettings;
pub mod lyrics;
pub mod queue;
pub mod state;
use crate::shell::shell::Shell;
use gtk::prelude::*;
use lyrics::state::{LyricsState, SelectedLyricsState};
use playback::PlaybackView;
use queue::QueueState;
use rufin_core::runtime::WaveformProjection;
use state::PlaybackState;
use std::cell::{Ref, RefCell};
use std::rc::{Rc, Weak};
pub struct SelectedPlaybackState {
    player: PlaybackView,
    waveform: WaveformProjection,
    seek_preview_seconds: Option<u32>,
}
pub struct PlayerUi {
    pub artwork: Rc<crate::widgets::artwork::ArtworkState>,
    pub control_feedback: Rc<crate::widgets::feedback::ControlFeedbackState>,
    catalog: std::sync::Arc<library::Catalog>,
    runtime: tokio::runtime::Handle,
    shell: Weak<Shell>,
    content_stack: gtk::glib::WeakRef<gtk::Stack>,
    pub settings: Rc<crate::widgets::settings::SettingsState>,
    pub playback_owner: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
    pub lyrics_service: rufin_core::lyrics::LyricsHandle,
    pub right_panel: right_panel::RightPanelWidgets,
    window: gtk::glib::WeakRef<gtk::ApplicationWindow>,
    toast_overlay: gtk::glib::WeakRef<adw::ToastOverlay>,
    pub views: PlayerDesktopWidgets,
    pub controls: PlaybackState,
    pub lyrics: LyricsState,
    playback: RefCell<Option<SelectedPlaybackState>>,
    queue: RefCell<QueueState>,
    pub lyric_content: RefCell<SelectedLyricsState>,
}
impl PlayerUi {
    pub fn new(
        controls: PlaybackState,
        lyrics: LyricsState,
        views: PlayerDesktopWidgets,
        right_panel: right_panel::RightPanelWidgets,
        settings: Rc<crate::widgets::settings::SettingsState>,
        playback_owner: std::sync::Arc<rufin_core::playback::PlaybackOwner>,
        lyrics_service: rufin_core::lyrics::LyricsHandle,
        window: &gtk::ApplicationWindow,
        toast_overlay: &adw::ToastOverlay,
        artwork: Rc<crate::widgets::artwork::ArtworkState>,
        control_feedback: Rc<crate::widgets::feedback::ControlFeedbackState>,
        catalog: std::sync::Arc<library::Catalog>,
        runtime: tokio::runtime::Handle,
        shell: Weak<Shell>,
        content_stack: &gtk::Stack,
    ) -> Self {
        Self {
            artwork,
            control_feedback,
            catalog,
            runtime,
            shell,
            content_stack: content_stack.downgrade(),
            settings,
            playback_owner,
            lyrics_service,
            right_panel,
            window: window.downgrade(),
            toast_overlay: toast_overlay.downgrade(),
            views,
            controls,
            lyrics,
            playback: RefCell::new(None),
            queue: RefCell::new(QueueState::new()),
            lyric_content: RefCell::new(SelectedLyricsState::new()),
        }
    }
    pub fn replace_player(&self, player: PlaybackView) {
        let mut playback = self.playback.borrow_mut();
        let previous_uri = playback
            .as_ref()
            .and_then(|playback| playback.player.transport.current.as_ref())
            .map(|current| current.row.track.media_uri.as_str());
        let next_uri = player
            .transport
            .current
            .as_ref()
            .map(|current| current.row.track.media_uri.as_str());
        let track_changed = previous_uri != next_uri;
        match playback.as_mut() {
            Some(playback) => playback.player = player,
            None => {
                *playback = Some(SelectedPlaybackState {
                    player,
                    waveform: WaveformProjection::default(),
                    seek_preview_seconds: None,
                });
            }
        }
        drop(playback);
        if track_changed {
            let controls = &self.views.player_controls;
            crate::widgets::favorites::set_favorite_button_active(&controls.favorite_button, false);
            controls.rating.set_rating(None, false);
        }
    }
    fn shell(&self) -> Option<Rc<Shell>> {
        self.shell.upgrade()
    }

    pub fn navigate(&self, route: rufin_core::route::Route) {
        if let Some(shell) = self.shell() {
            shell.navigate(route);
        }
    }

    /// Navigation for the artist and album links of the player.
    pub fn route_navigation(&self) -> Rc<dyn Fn(rufin_core::route::Route)> {
        let shell = self.shell.clone();
        Rc::new(move |route| {
            if let Some(shell) = shell.upgrade() {
                shell.navigate(route);
            }
        })
    }

    /// Shows the lyrics and visualizer panels as asked, opening the right sidebar for them.
    fn show_media_panels(&self, lyrics: bool, visualizer: bool) {
        if let Some(shell) = self.shell() {
            shell.set_right_panel_media_visibility(lyrics, visualizer);
        }
    }

    fn refresh_current_controls(&self) {
        if let Some(shell) = self.shell() {
            shell.refresh_bottom_player_owner_details();
        }
    }

    fn open_queue_menu(
        &self,
        target: &gtk::Widget,
        row: &library::QueueRow,
        selection: Option<queue::QueueSelectionSnapshot>,
        position: Option<(f64, f64)>,
    ) {
        let Some(shell) = self.shell() else { return };
        if let Some(selection) = selection {
            queue::present_queue_selection_context_menu(target, &shell, selection, position);
        } else {
            crate::menus::media_menus::present_track_menu(
                target,
                &shell,
                row.track.clone(),
                position,
                None,
                Some((row.position, row.occurrence_id)),
            );
        }
    }

    fn set_track_favorite(&self, media_uri: String, favorite: bool) {
        if let Some(shell) = self.shell() {
            shell.set_favorite(library::FavoriteTarget::Track(media_uri), favorite);
        }
    }

    fn related_tracks_view(&self, rows: Vec<library::TrackRow>) -> (gtk::Widget, Rc<dyn Fn()>) {
        let Some(shell) = self.shell() else {
            return (gtk::Box::default().upcast(), Rc::new(|| {}));
        };
        let route = shell.navigation.routes.borrow().current().clone();
        crate::library::CatalogUi::new(&shell, &route, None).related_tracks_view(rows)
    }

    fn present_full_artwork(&self, artwork: artwork::ArtworkBinding) {
        if let Some(shell) = self.shell() {
            shell.present_full_artwork(artwork);
        }
    }

    pub fn clear_player(&self) {
        self.playback.borrow_mut().take();
    }
    pub fn waveform(&self) -> Option<WaveformProjection> {
        self.playback
            .borrow()
            .as_ref()
            .map(|playback| playback.waveform.clone())
    }
    pub fn set_waveform(&self, waveform: WaveformProjection) {
        if let Some(playback) = self.playback.borrow_mut().as_mut() {
            playback.waveform = waveform;
        }
    }
    pub fn seek_preview_seconds(&self) -> Option<u32> {
        self.playback
            .borrow()
            .as_ref()
            .and_then(|playback| playback.seek_preview_seconds)
    }
    pub fn set_seek_preview_seconds(&self, seconds: Option<u32>) {
        if let Some(playback) = self.playback.borrow_mut().as_mut() {
            playback.seek_preview_seconds = seconds;
        }
    }
    pub fn selected_playback(&self) -> Option<Ref<'_, PlaybackView>> {
        Ref::filter_map(self.playback.borrow(), |playback| {
            playback.as_ref().map(|playback| &playback.player)
        })
        .ok()
    }
    pub fn selected_queue(&self) -> Option<Ref<'_, QueueState>> {
        Some(self.queue.borrow())
    }
    pub fn selected_lyrics(&self) -> Option<Ref<'_, SelectedLyricsState>> {
        Some(self.lyric_content.borrow())
    }
}

pub mod bottom;
mod color_chooser;
pub mod equalizer;
pub mod fullscreen;
pub mod fullscreen_background;
mod icons;
mod preset_name;
mod preset_transfer;
mod progress;
mod refill_ring;
pub mod visualizer;
mod visualizer_settings;
use bottom::PlayerControls;
use fullscreen::FullscreenPlayerParts;
use visualizer::VisualizerParts;
pub struct PlayerDesktopWidgets {
    pub fullscreen_player: FullscreenPlayerParts,
    pub player_controls: PlayerControls,
    pub visualizer: VisualizerParts,
}

pub mod right_panel;
impl PlayerUi {
    pub fn right_sidebar_visible(&self) -> bool {
        self.right_panel.right_panel_slot.get_visible()
    }
    pub fn fullscreen_player_visible(&self) -> bool {
        self.views.fullscreen_player.visible.get()
    }
}

impl PlayerUi {
    pub fn update_playback_settings(self: &Rc<Self>, update: impl FnOnce(&mut PlaybackSettings)) {
        if self
            .settings
            .update_app_settings("playback settings", |settings| {
                let previous = settings.playback.clone();
                update(&mut settings.playback);
                settings.playback.sanitize();
                settings.playback != previous
            })
            .is_some()
        {
            self.update_bottom_player();
        }
    }
}

pub mod outputs;

pub mod playback_settings;
