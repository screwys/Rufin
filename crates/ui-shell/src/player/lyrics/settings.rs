use crate::shell::Shell;
use adw::prelude::*;
use std::rc::Rc;
use ui_player::lyrics::settings::MediaSettingsPages;
pub(crate) fn connect_lyrics_settings_controls(shell: &Rc<Shell>) {
    let settings_shell = Rc::downgrade(shell);
    shell
        .player_ui
        .right_panel
        .visualizer_settings
        .connect_clicked(move |_| {
            if let Some(shell) = settings_shell.upgrade() {
                present_lyrics_settings_dialog(&shell, MediaSettingsPages::Visualizer);
            }
        });
    let Some(lyrics) = shell.player_ui.selected_lyrics() else {
        return;
    };
    for (pane, fullscreen) in [
        (lyrics.right_pane.clone(), false),
        (lyrics.fullscreen_pane.clone(), true),
    ] {
        let settings_shell = Rc::downgrade(shell);
        pane.connect_settings_clicked(move || {
            if let Some(shell) = settings_shell.upgrade() {
                let player = &shell.player_ui;
                let (lyrics, visualizer) = if fullscreen {
                    let parts = &player.views.fullscreen_player;
                    (parts.lyrics_enabled.get(), parts.visualizer_enabled.get())
                } else {
                    (
                        player.lyrics.panel_visible.get(),
                        player.right_panel.visualizer_visible.get()
                            && player.settings.current.borrow().right_panel.combined,
                    )
                };
                let pages = match (lyrics, visualizer) {
                    (true, true) => MediaSettingsPages::Combined,
                    (false, true) => MediaSettingsPages::Visualizer,
                    _ => MediaSettingsPages::Lyrics,
                };
                present_lyrics_settings_dialog(&shell, pages);
            }
        });
    }
}
fn present_lyrics_settings_dialog(shell: &Rc<Shell>, pages: MediaSettingsPages) {
    let weak = Rc::downgrade(shell);
    let uses_local_storage: Rc<dyn Fn() -> bool> = Rc::new(move || {
        weak.upgrade()
            .is_some_and(|shell| selected_source_uses_local_lyrics_storage(&shell))
    });
    let weak = Rc::downgrade(shell);
    let appearance_changed: Rc<dyn Fn()> = Rc::new(move || {
        if let Some(shell) = weak.upgrade() {
            shell.appearance.apply(&shell.settings.current.borrow());
        }
    });
    ui_player::lyrics::settings::present_lyrics_settings_dialog(
        &shell.player_ui,
        &shell.chrome.window,
        pages,
        uses_local_storage,
        appearance_changed,
    );
}
fn selected_source_uses_local_lyrics_storage(shell: &Shell) -> bool {
    let Some(source_id) = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_id.clone())
    else {
        return false;
    };
    shell
        .source
        .configured
        .borrow()
        .sources
        .iter()
        .find(|source| source.id == source_id)
        .is_some_and(|source| {
            source.kind == "local"
                || matches!(source.kind.as_str(), "navidrome" | "subsonic")
                    && selected_source_has_local_access(shell)
        })
}
fn selected_source_has_local_access(shell: &Shell) -> bool {
    let Some(source_id) = shell
        .selected_library()
        .as_deref()
        .map(|selected| selected.source_id.clone())
    else {
        return false;
    };
    shell
        .source
        .configured
        .borrow()
        .local_access
        .iter()
        .find(|summary| summary.source_id == source_id)
        .is_some_and(|summary| summary.access.is_some())
}
