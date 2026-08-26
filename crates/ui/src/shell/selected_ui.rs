use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use adw::prelude::*;
use playback::PlaybackView;

use crate::favorites::FavoriteSessionState;
use crate::player::lyrics::search::connect_lyrics_search_controls;
use crate::player::lyrics::state::SelectedLyricsState;
use crate::player::queue::QueueState;
use crate::player::queue::clear_queue_panel_children;
use crate::preferences::dialogs::popup::present_light_dismiss_dialog;
use crate::runtime::{SelectedLibrary, WaveformProjection};

use super::Shell;

/// Everything whose validity ends with the selected source.
///
/// `Shell` keeps one slot for this value, so releasing it drops the whole
/// selected-source UI graph instead of asking every component to remember its
/// own cleanup step.
pub(crate) struct SelectedUiSession {
    pub(crate) library: SelectedLibrary,
    pub(crate) playback: SelectedPlaybackState,
    pub(crate) queue: QueueState,
    pub(crate) lyrics: SelectedLyricsState,
    pub(crate) favorites: FavoriteSessionState,
    pub(crate) dialogs: SelectedDialogState,
    playlist_picker_refresh: RefCell<Option<Rc<dyn Fn()>>>,
}

impl SelectedUiSession {
    pub(crate) fn new(library: SelectedLibrary, player: PlaybackView) -> Self {
        Self {
            library,
            playback: SelectedPlaybackState {
                player,
                waveform: WaveformProjection::default(),
                seek_preview_seconds: None,
            },
            queue: QueueState::new(),
            lyrics: SelectedLyricsState::new(),
            favorites: FavoriteSessionState::default(),
            dialogs: SelectedDialogState::default(),
            playlist_picker_refresh: RefCell::new(None),
        }
    }
}

pub(crate) struct SelectedPlaybackState {
    player: PlaybackView,
    waveform: WaveformProjection,
    seek_preview_seconds: Option<u32>,
}

#[derive(Default)]
pub(crate) struct SelectedDialogState {
    dialogs: RefCell<Vec<adw::Dialog>>,
}

impl SelectedDialogState {
    fn register(&self, dialog: &adw::Dialog) {
        let mut dialogs = self.dialogs.borrow_mut();
        if !dialogs.iter().any(|current| current == dialog) {
            dialogs.push(dialog.clone());
        }
    }

    fn forget(&self, dialog: &adw::Dialog) {
        self.dialogs
            .borrow_mut()
            .retain(|current| current != dialog);
    }

    fn close_all(&self) {
        let dialogs = std::mem::take(&mut *self.dialogs.borrow_mut());
        for dialog in dialogs {
            dialog.force_close();
        }
    }
}

impl Drop for SelectedDialogState {
    fn drop(&mut self) {
        self.close_all();
    }
}

pub(crate) struct SelectedUiState {
    session: RefCell<Option<SelectedUiSession>>,
}

impl SelectedUiState {
    pub(crate) fn new() -> Self {
        Self {
            session: RefCell::new(None),
        }
    }

    pub(crate) fn session(&self) -> Option<Ref<'_, SelectedUiSession>> {
        Ref::filter_map(self.session.borrow(), Option::as_ref).ok()
    }

    pub(crate) fn session_mut(&self) -> Option<RefMut<'_, SelectedUiSession>> {
        RefMut::filter_map(self.session.borrow_mut(), Option::as_mut).ok()
    }

    pub(crate) fn install(&self, session: SelectedUiSession) {
        let mut current = self.session.borrow_mut();
        assert!(
            current.is_none(),
            "a selected UI session must be released before installing its replacement"
        );
        *current = Some(session);
    }

    pub(crate) fn take(&self) -> Option<SelectedUiSession> {
        self.session.borrow_mut().take()
    }

    pub(crate) fn replace_library(&self, library: SelectedLibrary) {
        let mut session = self
            .session_mut()
            .expect("a Library replacement requires its selected UI session");
        session.library = library;
    }

    pub(crate) fn replace_player(&self, player: PlaybackView) {
        let mut session = self
            .session_mut()
            .expect("a PlaybackView replacement requires its selected UI session");
        session.playback.player = player;
    }

    pub(crate) fn waveform(&self) -> Option<WaveformProjection> {
        self.session()
            .map(|session| session.playback.waveform.clone())
    }

    pub(crate) fn set_waveform(&self, waveform: WaveformProjection) {
        if let Some(mut session) = self.session_mut() {
            session.playback.waveform = waveform;
        }
    }

    pub(crate) fn seek_preview_seconds(&self) -> Option<u32> {
        self.session()
            .and_then(|session| session.playback.seek_preview_seconds)
    }

    pub(crate) fn set_seek_preview_seconds(&self, seconds: Option<u32>) {
        if let Some(mut session) = self.session_mut() {
            session.playback.seek_preview_seconds = seconds;
        }
    }
}

impl Shell {
    pub(crate) fn set_playlist_picker_refresh(&self, refresh: Option<Rc<dyn Fn()>>) {
        if let Some(session) = self.selected_ui.session() {
            session.playlist_picker_refresh.replace(refresh);
        }
    }

    pub(crate) fn refresh_playlist_picker(&self) {
        if let Some(session) = self.selected_ui.session()
            && let Some(refresh) = session.playlist_picker_refresh.borrow().as_ref()
        {
            refresh();
        }
    }

    pub(crate) fn selected_library(&self) -> Option<Ref<'_, SelectedLibrary>> {
        self.selected_ui
            .session()
            .map(|session| Ref::map(session, |session| &session.library))
    }

    pub(crate) fn selected_playback(&self) -> Option<Ref<'_, PlaybackView>> {
        self.selected_ui
            .session()
            .map(|session| Ref::map(session, |session| &session.playback.player))
    }

    pub(crate) fn selected_queue(&self) -> Option<Ref<'_, QueueState>> {
        self.selected_ui
            .session()
            .map(|session| Ref::map(session, |session| &session.queue))
    }

    pub(crate) fn selected_lyrics(&self) -> Option<Ref<'_, SelectedLyricsState>> {
        self.selected_ui
            .session()
            .map(|session| Ref::map(session, |session| &session.lyrics))
    }

    pub(crate) fn present_selected_dialog<D>(self: &Rc<Self>, dialog: &D)
    where
        D: IsA<adw::Dialog> + Clone + 'static,
    {
        let dialog = dialog.clone().upcast::<adw::Dialog>();
        let Some(session) = self.selected_ui.session() else {
            return;
        };
        session.dialogs.register(&dialog);
        drop(session);

        let shell = Rc::downgrade(self);
        dialog.connect_closed(move |dialog| {
            let Some(shell) = shell.upgrade() else {
                return;
            };
            if let Some(session) = shell.selected_ui.session() {
                session.dialogs.forget(dialog);
            }
        });
        present_light_dismiss_dialog(&dialog, &self.chrome.window);
    }

    pub(crate) fn attach_selected_ui_roots(self: &Rc<Self>) {
        let Some(session) = self.selected_ui.session() else {
            return;
        };
        self.right_panel
            .lyrics_host
            .append(session.lyrics.right_pane.widget());
        self.player_view
            .fullscreen_player
            .lyrics_host
            .append(session.lyrics.fullscreen_pane.widget());
        drop(session);
        connect_lyrics_search_controls(self);
    }

    pub(crate) fn detach_selected_ui_roots(&self) {
        if let Some(session) = self.selected_ui.session() {
            if session.lyrics.right_pane.widget().parent().is_some() {
                self.right_panel
                    .lyrics_host
                    .remove(session.lyrics.right_pane.widget());
            }
            if session.lyrics.fullscreen_pane.widget().parent().is_some() {
                self.player_view
                    .fullscreen_player
                    .lyrics_host
                    .remove(session.lyrics.fullscreen_pane.widget());
            }
        }
        clear_queue_panel_children(&self.right_panel.queue_panel);
        clear_queue_panel_children(&self.player_view.fullscreen_player.queue_panel);
    }
}
