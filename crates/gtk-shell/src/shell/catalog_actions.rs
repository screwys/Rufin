use super::Shell;
use playback::QueuePlacement;
use rufin_core::playback::PlaybackTarget;
use std::rc::Rc;
use tracing::warn;
pub(crate) fn download(target: &PlaybackTarget, shell: &Shell) {
    let selected = shell.selected_library();
    let scope = selected.as_ref().map(|selected| selected.source_key);
    let folder = selected
        .as_ref()
        .and_then(|selected| selected.music_folder_key);
    let result = shell
        .products
        .source
        .download_target(target.clone(), scope, folder);
    let target = target.clone();
    shell.products.runtime.spawn(async move {
        if let Err(error) = result
            .recv()
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result)
        {
            warn!(target = ?target, %error, "failed to identify download tracks");
        }
    });
}

pub(crate) fn remove_download(target: &PlaybackTarget, shell: &Shell) {
    let selected = shell.selected_library();
    let scope = selected.as_ref().map(|selected| selected.source_key);
    let folder = selected
        .as_ref()
        .and_then(|selected| selected.music_folder_key);
    let result = shell
        .products
        .source
        .remove_download_target(target.clone(), scope, folder);
    let target = target.clone();
    shell.products.runtime.spawn(async move {
        if let Err(error) = result
            .recv()
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result)
        {
            warn!(target = ?target, %error, "failed to identify downloaded tracks");
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
                    artwork: gtk_widgets::downloads::OperationArtwork::MediaUris(
                        preview_uris.clone(),
                    ),
                    item_count: accepted,
                    kind: gtk_widgets::downloads::OperationFeedbackKind::PlaylistAdded {
                        destination: destination.clone(),
                    },
                });
            }
        }),
    );
}
