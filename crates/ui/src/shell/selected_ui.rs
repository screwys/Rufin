use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;

use adw::prelude::*;
use playback::PlaybackView;

use crate::favorites::FavoriteSessionState;
use crate::player::lyrics::search::connect_lyrics_search_controls;
use crate::player::lyrics::state::SelectedLyricsState;
use crate::player::queue::QueueState;
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
    pub(crate) favorites: FavoriteSessionState,
    pub(crate) dialogs: SelectedDialogState,
}

impl SelectedUiSession {
    pub(crate) fn new(library: SelectedLibrary) -> Self {
        Self {
            library,
            favorites: FavoriteSessionState::default(),
            dialogs: SelectedDialogState::default(),
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
    dialogs: RefCell<Vec<gtk::glib::WeakRef<adw::Dialog>>>,
}

impl SelectedDialogState {
    fn register(&self, dialog: &adw::Dialog) {
        let mut dialogs = self.dialogs.borrow_mut();
        dialogs.retain(|current| current.upgrade().is_some());
        if !dialogs
            .iter()
            .filter_map(gtk::glib::WeakRef::upgrade)
            .any(|current| current == *dialog)
        {
            dialogs.push(dialog.downgrade());
        }
    }
}

impl Drop for SelectedDialogState {
    fn drop(&mut self) {
        for dialog in self.dialogs.get_mut().drain(..) {
            if let Some(dialog) = dialog.upgrade() {
                dialog.force_close();
            }
        }
    }
}

pub(crate) struct SelectedUiState {
    session: RefCell<Option<SelectedUiSession>>,
    pub(crate) metadata_load: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    playback: RefCell<Option<SelectedPlaybackState>>,
    queue: RefCell<QueueState>,
    lyrics: RefCell<SelectedLyricsState>,
    playlist_picker_refresh: RefCell<Option<Rc<dyn Fn()>>>,
}

impl SelectedUiState {
    pub(crate) fn new() -> Self {
        Self {
            session: RefCell::new(None),
            metadata_load: RefCell::new(None),
            playback: RefCell::new(None),
            queue: RefCell::new(QueueState::new()),
            lyrics: RefCell::new(SelectedLyricsState::new()),
            playlist_picker_refresh: RefCell::new(None),
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
        if let Some(load) = self.metadata_load.borrow_mut().take() {
            load.abort();
        }
        let session = self.session.borrow_mut().take();
        if session.is_some() {
            self.playlist_picker_refresh.borrow_mut().take();
        }
        session
    }

    pub(crate) fn replace_library(&self, library: SelectedLibrary) {
        let mut session = self
            .session_mut()
            .expect("a Library replacement requires its selected UI session");
        session.library = library;
    }

    pub(crate) fn replace_player(&self, player: PlaybackView) {
        let mut playback = self.playback.borrow_mut();
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
    }

    pub(crate) fn waveform(&self) -> Option<WaveformProjection> {
        self.playback
            .borrow()
            .as_ref()
            .map(|playback| playback.waveform.clone())
    }

    pub(crate) fn set_waveform(&self, waveform: WaveformProjection) {
        if let Some(playback) = self.playback.borrow_mut().as_mut() {
            playback.waveform = waveform;
        }
    }

    pub(crate) fn seek_preview_seconds(&self) -> Option<u32> {
        self.playback
            .borrow()
            .as_ref()
            .and_then(|playback| playback.seek_preview_seconds)
    }

    pub(crate) fn set_seek_preview_seconds(&self, seconds: Option<u32>) {
        if let Some(playback) = self.playback.borrow_mut().as_mut() {
            playback.seek_preview_seconds = seconds;
        }
    }
}

impl Shell {
    pub(crate) fn set_playlist_picker_refresh(&self, refresh: Option<Rc<dyn Fn()>>) {
        self.selected_ui.playlist_picker_refresh.replace(refresh);
    }

    pub(crate) fn refresh_playlist_picker(&self) {
        let refresh = self.selected_ui.playlist_picker_refresh.borrow().clone();
        if let Some(refresh) = refresh {
            refresh();
        }
    }

    pub(crate) fn selected_library(&self) -> Option<Ref<'_, SelectedLibrary>> {
        self.selected_ui
            .session()
            .map(|session| Ref::map(session, |session| &session.library))
    }

    pub(crate) fn selected_playback(&self) -> Option<Ref<'_, PlaybackView>> {
        Ref::filter_map(self.selected_ui.playback.borrow(), |playback| {
            playback.as_ref().map(|playback| &playback.player)
        })
        .ok()
    }

    pub(crate) fn selected_queue(&self) -> Option<Ref<'_, QueueState>> {
        Some(self.selected_ui.queue.borrow())
    }

    pub(crate) fn selected_lyrics(&self) -> Option<Ref<'_, SelectedLyricsState>> {
        Some(self.selected_ui.lyrics.borrow())
    }

    pub(crate) fn present_selected_dialog<D>(self: &Rc<Self>, dialog: &D)
    where
        D: IsA<adw::Dialog> + Clone + 'static,
    {
        let dialog = dialog.clone().upcast::<adw::Dialog>();
        if let Some(session) = self.selected_ui.session() {
            session.dialogs.register(&dialog);
        }

        present_light_dismiss_dialog(&dialog, &self.chrome.window);
    }

    pub(crate) fn attach_selected_ui_roots(self: &Rc<Self>) {
        let lyrics = self.selected_ui.lyrics.borrow();
        self.right_panel
            .lyrics_host
            .append(lyrics.right_pane.widget());
        self.player_view
            .fullscreen_player
            .lyrics_host
            .append(lyrics.fullscreen_pane.widget());
        drop(lyrics);
        connect_lyrics_search_controls(self);
    }
}
