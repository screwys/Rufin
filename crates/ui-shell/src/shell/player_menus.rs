use crate::shell::Shell;
use adw::prelude::*;
use std::rc::Rc;
use ui_player::state::current_playback_track;
use ui_shared::interactions::install_context_menu_openers;
use ui_shared::media_menus::present_playback_media_menu;
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
                present_playback_media_menu(
                    target,
                    &shell.media_menus,
                    track.media_uri.clone(),
                    Some(track),
                    position,
                    None,
                    None,
                );
            }
        }),
    );
}

pub(crate) fn present_current_track_context_menu(
    target: &impl IsA<gtk::Widget>,
    shell: &Rc<Shell>,
) {
    if let Some(track) = current_playback_track(shell.player_ui.selected_playback().as_deref()) {
        present_playback_media_menu(
            target.as_ref(),
            &shell.media_menus,
            track.media_uri.clone(),
            Some(track),
            None,
            Some(gtk::PositionType::Top),
            None,
        );
    }
}

pub(crate) fn present_queue_track_context_menu(
    target: &gtk::Widget,
    shell: &Rc<Shell>,
    media: library::QueueItem,
    occurrence: playback::OccurrenceId,
    position: Option<(f64, f64)>,
) {
    present_playback_media_menu(
        target,
        &shell.media_menus,
        media.media_uri.clone(),
        Some(media),
        position,
        None,
        Some(occurrence),
    );
}
