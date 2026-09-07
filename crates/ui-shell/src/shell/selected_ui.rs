use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;
use ui_shared::popup::SelectedDialogState;

use adw::prelude::*;

use rufin_core::runtime::SelectedLibrary;
use ui_player::lyrics::search::connect_lyrics_search_controls;
use ui_shared::favorites::FavoriteSessionState;
use ui_shared::popup::present_light_dismiss_dialog;

use super::Shell;

/// Everything whose validity ends with the selected source.
///
/// `Shell` keeps one slot for this value, so releasing it drops the whole
/// selected-source UI graph instead of asking every component to remember its
/// own cleanup step.
pub(crate) struct SelectedUiSession {
    pub(crate) library: SelectedLibrary,
    pub(crate) favorites: Rc<FavoriteSessionState>,
    pub(crate) dialogs: SelectedDialogState,
}

impl SelectedUiSession {
    pub(crate) fn new(library: SelectedLibrary) -> Self {
        Self {
            library,
            favorites: Rc::new(FavoriteSessionState::default()),
            dialogs: SelectedDialogState::default(),
        }
    }
}

pub(crate) struct SelectedUiState {
    session: RefCell<Option<SelectedUiSession>>,
    pub(crate) metadata_load: RefCell<Option<gtk::glib::JoinHandle<()>>>,
    playlist_picker_refresh: RefCell<Option<Rc<dyn Fn()>>>,
}

impl SelectedUiState {
    pub(crate) fn new() -> Self {
        Self {
            session: RefCell::new(None),
            metadata_load: RefCell::new(None),
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
        let lyrics = self.player_ui.lyric_content.borrow();
        self.player_ui
            .right_panel
            .lyrics_host
            .append(lyrics.right_pane.widget());
        self.player_ui
            .views
            .fullscreen_player
            .lyrics_host
            .append(lyrics.fullscreen_pane.widget());
        drop(lyrics);
        connect_lyrics_search_controls(&self.player_ui);
        crate::player::lyrics::settings::connect_lyrics_settings_controls(self);
    }
}
