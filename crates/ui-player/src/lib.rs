//! Persistent playback, queue and lyrics presentation.
use playback::{EqualizerSettings, PlaybackSettings};
pub mod lyrics;
pub mod queue;
pub mod state;
pub mod ui_resource;
use gtk::prelude::*;
use lyrics::state::{LyricsState, SelectedLyricsState};
use playback::PlaybackView;
use queue::QueueState;
use rufin_core::runtime::WaveformProjection;
use state::PlaybackState;
use std::cell::{Ref, RefCell};
use std::rc::Rc;
pub struct SelectedPlaybackState {
    player: PlaybackView,
    waveform: WaveformProjection,
    seek_preview_seconds: Option<u32>,
}
pub struct PlayerUi {
    pub artwork: Rc<ui_shared::artwork::ArtworkState>,
    pub control_feedback: Rc<ui_shared::feedback::ControlFeedbackState>,
    database: std::sync::Arc<library::Database>,
    runtime: tokio::runtime::Handle,
    navigate: Rc<dyn Fn(ui_shared::route::Route)>,
    refresh_current_controls: Rc<dyn Fn()>,
    open_queue_menu: QueueMenu,
    set_track_favorite: TrackFavorite,
    content_stack: gtk::glib::WeakRef<gtk::Stack>,
    pub settings: Rc<ui_shared::settings::SettingsState>,
    pub playback_handles: playback::PlaybackHandles,
    pub lyrics_service: rufin_core::lyrics::LyricsHandle,
    pub right_panel: right_panel::RightPanelWidgets,
    window: gtk::glib::WeakRef<gtk::ApplicationWindow>,
    toast_overlay: gtk::glib::WeakRef<adw::ToastOverlay>,
    panel_slot: gtk::glib::WeakRef<gtk::ScrolledWindow>,
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
        settings: Rc<ui_shared::settings::SettingsState>,
        playback_handles: playback::PlaybackHandles,
        lyrics_service: rufin_core::lyrics::LyricsHandle,
        window: &gtk::ApplicationWindow,
        toast_overlay: &adw::ToastOverlay,
        panel_slot: &gtk::ScrolledWindow,
        artwork: Rc<ui_shared::artwork::ArtworkState>,
        control_feedback: Rc<ui_shared::feedback::ControlFeedbackState>,
        database: std::sync::Arc<library::Database>,
        runtime: tokio::runtime::Handle,
        navigate: Rc<dyn Fn(ui_shared::route::Route)>,
        refresh_current_controls: Rc<dyn Fn()>,
        open_queue_menu: QueueMenu,
        set_track_favorite: TrackFavorite,
        content_stack: &gtk::Stack,
    ) -> Self {
        Self {
            artwork,
            control_feedback,
            database,
            runtime,
            navigate,
            refresh_current_controls,
            open_queue_menu,
            set_track_favorite,
            content_stack: content_stack.downgrade(),
            settings,
            playback_handles,
            lyrics_service,
            right_panel,
            window: window.downgrade(),
            toast_overlay: toast_overlay.downgrade(),
            panel_slot: panel_slot.downgrade(),
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
            .map(|current| current.media_uri.as_str());
        let next_uri = player
            .transport
            .current
            .as_ref()
            .map(|current| current.media_uri.as_str());
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
            ui_shared::favorites::set_favorite_button_active(&controls.favorite_button, false);
            controls.rating.set_rating(None, false);
        }
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
pub fn register_resources() -> Result<(), String> {
    ui_shared::register_resources()?;
    static REGISTERED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    REGISTERED
        .get_or_init(|| {
            gtk::gio::resources_register_include!("ui-player.gresource")
                .map_err(|error| error.to_string())
        })
        .clone()
}

pub mod bottom;
pub mod equalizer;
pub mod fullscreen;
mod icons;
mod progress;
pub mod visualizer;
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
        self.panel_slot
            .upgrade()
            .is_some_and(|slot| slot.get_visible())
    }
    pub fn fullscreen_player_visible(&self) -> bool {
        self.views.fullscreen_player.visible.get()
    }
}

impl PlayerUi {
    pub fn update_playback_settings(self: &Rc<Self>, update: impl FnOnce(&mut PlaybackSettings)) {
        if let Some(settings) = self
            .settings
            .update_app_settings("playback settings", |settings| {
                let previous = settings.playback.clone();
                update(&mut settings.playback);
                settings.playback.sanitize();
                settings.playback != previous
            })
        {
            self.sync_fullscreen_equalizer_controls(&settings.playback.equalizer);
            self.update_bottom_player();
        }
    }
    pub fn sync_fullscreen_equalizer_controls(&self, equalizer: &EqualizerSettings) {
        self.views
            .fullscreen_player
            .equalizer
            .set_settings(equalizer);
    }
}

pub mod outputs;

pub type QueueMenu = Rc<
    dyn Fn(
        &gtk::Widget,
        library::QueuePageRow,
        Option<queue::QueueSelectionSnapshot>,
        Option<(f64, f64)>,
    ),
>;
pub type TrackFavorite = Rc<dyn Fn(String, bool, &gtk::Button)>;

pub mod playback_settings;
