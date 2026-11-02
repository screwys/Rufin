use crate::menus::media_menus::present_track_menu;
use crate::player::state::current_playback_track;
use crate::shell::shell::Shell;
use crate::widgets::interactions::install_context_menu_openers;
use adw::prelude::*;
use std::rc::Rc;
pub(crate) fn install_current_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
) {
    let shell = Rc::clone(shell);
    install_context_menu_openers(
        target,
        Rc::new(move |target, position| {
            if let Some(track) =
                current_playback_track(shell.player_ui.selected_playback().as_deref())
            {
                present_track_menu(target, &shell, track, position, None, None);
            }
        }),
    );
}

pub(crate) fn present_current_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
) {
    if let Some(track) = current_playback_track(shell.player_ui.selected_playback().as_deref()) {
        present_track_menu(
            target.as_ref(),
            &shell,
            track,
            None,
            Some(gtk::PositionType::Top),
            None,
        );
    }
}
