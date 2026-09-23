use super::Shell;
use gtk_widgets::media_drag::download_subject;
use playback::QueuePlacement;
use rufin_core::playback::PlaybackTarget;
use std::{rc::Rc, sync::Arc};
use tracing::warn;
pub(crate) fn download(target: &PlaybackTarget, shell: &Shell) {
    let selected = shell.selected_library();
    let scope = selected.as_ref().map(|selected| selected.source_key);
    let folder = selected
        .as_ref()
        .and_then(|selected| selected.music_folder_key);
    let database = Arc::clone(&shell.products.library);
    let target = target.clone();
    let source = shell.products.source.clone();
    shell.products.runtime.spawn(async move {
        match target.resolve_media_uris(&database, scope, folder).await {
            Ok(media_uris) => {
                source.download_media(download_subject(&target), media_uris);
            }
            Err(error) => warn!(target = ?target, %error, "failed to identify download tracks"),
        }
    });
}

pub(crate) fn remove_download(target: &PlaybackTarget, shell: &Shell) {
    let selected = shell.selected_library();
    let scope = selected.as_ref().map(|selected| selected.source_key);
    let folder = selected
        .as_ref()
        .and_then(|selected| selected.music_folder_key);
    let database = Arc::clone(&shell.products.library);
    let target = target.clone();
    let downloads = shell.products.downloads.clone();
    shell.products.runtime.spawn(async move {
        match target.resolve_media_uris(&database, scope, folder).await {
            Ok(order) => downloads.remove(order, true),
            Err(error) => {
                warn!(target = ?target, %error, "failed to identify downloaded tracks")
            }
        }
    });
}

pub(crate) fn play_target(
    target: &PlaybackTarget,
    shell: &Shell,
    placement: QueuePlacement,
    shuffled: bool,
) {
    let selected = shell.selected_library();
    let source = selected.as_ref().map(|selected| selected.source_key);
    let folder = selected
        .as_ref()
        .and_then(|selected| selected.music_folder_key);
    shell.products.playback.queue.play(
        playback::PlayRequest::ordered(target.queue_input(source, folder), 0, placement, true)
            .shuffled(shuffled),
    );
}

use downloads::DownloadSubject;
use gtk_player::state::current_playback_track;
pub(super) fn add_current_to_playlist(
    shell: &Rc<Shell>,
    playlist: library::PlaylistKey,
    destination: String,
) {
    let Some(media) = current_playback_track(shell.player_ui.selected_playback().as_deref()) else {
        return;
    };
    let subject = DownloadSubject::Prepared {
        context_id: "playlist-add-current".to_string(),
        title: Some(media.title.clone()),
    };
    let feedback_shell = Rc::downgrade(shell);
    let destination = destination.clone();
    let preview_uris = vec![media.media_uri.clone()];
    gtk_widgets::playlists::add_media_to_playlist(
        &shell.products.source,
        playlist,
        vec![media.media_uri],
        false,
        Rc::new(move |accepted| {
            if accepted > 0
                && let Some(shell) = feedback_shell.upgrade()
            {
                shell.show_operation_feedback(&gtk_widgets::downloads::OperationFeedback {
                    subject: subject.clone(),
                    preview_uris: preview_uris.clone(),
                    item_count: accepted,
                    kind: gtk_widgets::downloads::OperationFeedbackKind::PlaylistAdded {
                        destination: destination.clone(),
                    },
                });
            }
        }),
    );
}
