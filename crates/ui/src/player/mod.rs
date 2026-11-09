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
use std::cell::{Cell, Ref, RefCell};
use std::rc::{Rc, Weak};
pub struct SelectedPlaybackState {
    player: PlaybackView,
    waveform: WaveformProjection,
    seek_preview_seconds: Option<u32>,
}
/// Window content narrower than this shows only the mini player.
pub const MINI_PLAYER_MAX_WIDTH: i32 = 360;
/// Window content shorter than this shows only the mini player.
pub const MINI_PLAYER_MAX_HEIGHT: i32 = 240;
/// The short mini player shows the cover beside the details only with this much room above the
/// strip.
pub const MINI_PLAYER_PANE_MIN_HEIGHT: i32 = 80;

/// When the window is too small for pages, it shows only playback: the mini player.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MiniPlayer {
    #[default]
    Off,
    /// Short windows show the cover beside the details over the bar's strip, or the strip alone
    /// when there is no room.
    Short,
    /// Narrow, tall windows show the cover above the bar.
    Tall,
}

impl MiniPlayer {
    pub fn for_content(width: i32, height: i32) -> Self {
        if height < MINI_PLAYER_MAX_HEIGHT {
            Self::Short
        } else if width < MINI_PLAYER_MAX_WIDTH {
            Self::Tall
        } else {
            Self::Off
        }
    }
}

pub struct PlayerUi {
    pub control_feedback: Rc<crate::widgets::feedback::ControlFeedbackState>,
    shell: Weak<Shell>,
    content_stack: gtk::glib::WeakRef<gtk::Stack>,
    pub right_panel: right_panel::RightPanelWidgets,
    pub views: PlayerDesktopWidgets,
    pub controls: PlaybackState,
    pub lyrics: LyricsState,
    playback: RefCell<Option<SelectedPlaybackState>>,
    queue: RefCell<QueueState>,
    pub lyric_content: RefCell<SelectedLyricsState>,
    pub mini: Cell<MiniPlayer>,
    /// The tall mini player opened the fullscreen player, so leaving it closes it again.
    fullscreen_for_mini: Cell<bool>,
}
impl PlayerUi {
    pub fn new(
        controls: PlaybackState,
        lyrics: LyricsState,
        views: PlayerDesktopWidgets,
        right_panel: right_panel::RightPanelWidgets,
        control_feedback: Rc<crate::widgets::feedback::ControlFeedbackState>,
        shell: Weak<Shell>,
        content_stack: &gtk::Stack,
    ) -> Self {
        Self {
            control_feedback,
            shell,
            content_stack: content_stack.downgrade(),
            right_panel,
            views,
            controls,
            lyrics,
            playback: RefCell::new(None),
            queue: RefCell::new(QueueState::new()),
            lyric_content: RefCell::new(SelectedLyricsState::new()),
            mini: Cell::new(MiniPlayer::Off),
            fullscreen_for_mini: Cell::new(false),
        }
    }

    /// Switches between the full window and the mini player. The tall mini player shows the
    /// fullscreen player's cover above the bar; the short one shows the bar alone.
    pub fn apply_mini_player(self: &Rc<Self>, mini: MiniPlayer) {
        if self.mini.replace(mini) == mini {
            return;
        }
        self.views.fullscreen_player.mini.set(mini);
        // The mini player only plays: no menus, drags or fullscreen from the cover.
        self.views
            .player_controls
            .cover
            .area
            .set_can_target(mini == MiniPlayer::Off);
        // The mini players show the fullscreen player as the window's content.
        let mini_content = mini != MiniPlayer::Off;
        if mini_content && !self.fullscreen_player_visible() {
            self.fullscreen_for_mini.set(true);
            self.show_fullscreen_player(false);
        } else if !mini_content && self.fullscreen_for_mini.replace(false) {
            self.hide_fullscreen_player(None, false);
        } else {
            self.refresh_fullscreen_mode();
        }
        self.apply_bottom_player_layout();
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
            crate::favorites::set_favorite_button_active(&controls.favorite_button, false);
            controls.rating.set_rating(None, false);
        }
    }
    /// The window, while it is open.
    pub(crate) fn shell(&self) -> Option<Rc<Shell>> {
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
        crate::library::CatalogUi::new(&shell).related_tracks_view(rows)
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
        let Some(shell) = self.shell() else {
            return;
        };
        if shell
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
pub mod sleep_timer;
